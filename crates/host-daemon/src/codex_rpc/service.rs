use std::sync::{Arc, OnceLock};

use codex_app_server::{
    CodexAppServer, Error as AppServerError, RawResponse, RemoteError, ServerResponse,
};
use host_protocol::{
    RpcError, RpcId, RpcMessage, RpcNotification, RpcOutcome, RpcRequest, RpcResponse,
};
use serde_json::{json, Map, Value};
use tokio::sync::broadcast;

use super::routing::{
    CodexSession, ResponseDisposition, ResponseRoute, RouteError, SessionId, SessionRouter,
};
use crate::{
    DesktopProjectError, DesktopProjectStore, HOST_PROJECT_LIST_METHOD, HOST_THREAD_LIST_METHOD,
    HOST_THREAD_READ_METHOD, HOST_THREAD_START_METHOD,
};

/// Errors produced by the gateway itself. Errors from Codex are encoded as a
/// normal `RpcResponse::Failure` and delivered to the requesting session so
/// the raw error shape is not lost.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("RPC session {0} is not open")]
    UnknownSession(SessionId),
    #[error("RPC session {session} outbound queue is full")]
    QueueFull { session: SessionId },
    #[error("Codex App Server response failed: {0}")]
    Upstream(#[source] Box<AppServerError>),
    #[error("RPC response could not be encoded: {0}")]
    Serialization(String),
    #[error("method {method} is owned by the host daemon")]
    DaemonOwnedMethod { method: String },
}

impl From<RouteError> for DispatchError {
    fn from(error: RouteError) -> Self {
        match error {
            RouteError::UnknownSession(session) => Self::UnknownSession(session),
            RouteError::QueueFull { session } => Self::QueueFull { session },
        }
    }
}

/// Routes raw Codex RPC messages and explicit Host read projections to
/// authenticated mobile sessions.
#[derive(Clone)]
pub struct CodexRpcService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    app_server: Arc<CodexAppServer>,
    desktop_projects: DesktopProjectStore,
    router: SessionRouter,
    event_pump_started: OnceLock<()>,
}

impl CodexRpcService {
    pub fn new(app_server: Arc<CodexAppServer>, desktop_projects: DesktopProjectStore) -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                app_server,
                desktop_projects,
                router: SessionRouter::new(),
                event_pump_started: OnceLock::new(),
            }),
        }
    }

    /// Open a session with a bounded outbound queue.
    ///
    /// The event pump starts lazily because construction may happen before a
    /// Tokio runtime is entered.
    pub fn open_session(&self, capacity: usize) -> CodexSession {
        self.start_event_pump();
        self.inner.router.open_session(capacity)
    }

    /// Unregister a session explicitly. Dropping `CodexSession` has the same
    /// effect.
    pub fn close_session(&self, session: SessionId) {
        self.inner.router.close_session(session);
    }

    /// Forward a mobile request to Codex, preserving raw response and error
    /// extensions. Lifecycle and `host/*` projection methods remain
    /// daemon-owned.
    pub async fn dispatch_request(
        &self,
        session: SessionId,
        request: RpcRequest,
    ) -> Result<(), DispatchError> {
        self.ensure_session(session)?;
        let RpcRequest {
            id,
            method,
            params,
            extensions,
            ..
        } = request;
        let response = self
            .build_request_response(id, &method, params, extensions)
            .await;
        self.send_response(session, response)
    }

    /// Forward a mobile notification to Codex. Lifecycle notifications are
    /// emitted only by App Server startup.
    pub async fn dispatch_notification(
        &self,
        session: SessionId,
        notification: RpcNotification,
    ) -> Result<(), DispatchError> {
        self.ensure_session(session)?;
        if is_daemon_lifecycle(&notification.method) {
            return Err(DispatchError::DaemonOwnedMethod {
                method: notification.method,
            });
        }
        self.inner
            .app_server
            .notify_json(
                &notification.method,
                notification.params,
                notification.extensions,
            )
            .await
            .map_err(|error| DispatchError::Upstream(Box::new(error)))
    }

    /// Accept a response to a Codex-originated request. The first session to
    /// resolve a proxy id wins; every alias is removed before upstream I/O.
    pub async fn dispatch_response(
        &self,
        session: SessionId,
        response: RpcResponse,
    ) -> Result<ResponseDisposition, DispatchError> {
        self.ensure_session(session)?;
        let upstream_id = match self
            .inner
            .router
            .resolve_response(session, response.id.clone())
        {
            ResponseRoute::Forward(upstream_id) => upstream_id,
            ResponseRoute::Unknown => return Ok(ResponseDisposition::Unknown),
        };

        let (payload, extensions) = server_response(&response)?;
        self.inner
            .app_server
            .respond_json(upstream_id, payload, extensions)
            .await
            .map_err(|error| DispatchError::Upstream(Box::new(error)))?;
        Ok(ResponseDisposition::Accepted)
    }

    fn ensure_session(&self, session: SessionId) -> Result<(), DispatchError> {
        self.inner
            .router
            .ensure_session(session)
            .map_err(Into::into)
    }

    fn send_response(
        &self,
        session: SessionId,
        response: RpcResponse,
    ) -> Result<(), DispatchError> {
        self.inner
            .router
            .send_message(session, RpcMessage::Response(response))
            .map_err(Into::into)
    }

    async fn build_request_response(
        &self,
        id: RpcId,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
    ) -> RpcResponse {
        match classify_request(method) {
            RequestRoute::DaemonLifecycle => daemon_owned_response(id, method, extensions),
            RequestRoute::DesktopProjectList => {
                self.desktop_project_response(id, params, extensions).await
            }
            RequestRoute::Upstream {
                method: upstream_method,
                enrich_threads,
            } => {
                self.upstream_response(id, upstream_method, params, extensions, enrich_threads)
                    .await
            }
        }
    }

    async fn desktop_project_response(
        &self,
        id: RpcId,
        params: Value,
        extensions: Map<String, Value>,
    ) -> RpcResponse {
        let outcome = match self.inner.desktop_projects.project_list(&params).await {
            Ok(result) => RpcOutcome::Success { result },
            Err(error) => desktop_project_failure(error),
        };
        RpcResponse {
            id,
            outcome,
            extensions,
        }
    }

    async fn upstream_response(
        &self,
        id: RpcId,
        method: &str,
        params: Value,
        extensions: Map<String, Value>,
        enrich_threads: bool,
    ) -> RpcResponse {
        let (outcome, response_extensions) = app_server_response(
            self.inner
                .app_server
                .request_json_with_extensions(method, params, extensions)
                .await,
        );
        let outcome = if enrich_threads {
            self.enrich_thread_outcome(outcome).await
        } else {
            outcome
        };
        RpcResponse {
            id,
            outcome,
            extensions: response_extensions,
        }
    }

    async fn enrich_thread_outcome(&self, outcome: RpcOutcome) -> RpcOutcome {
        match outcome {
            RpcOutcome::Success { result } => {
                match self.inner.desktop_projects.enrich_threads(result).await {
                    Ok(enriched) => RpcOutcome::Success { result: enriched },
                    Err(error) => desktop_project_failure(error),
                }
            }
            failure => failure,
        }
    }

    fn start_event_pump(&self) {
        if self.inner.event_pump_started.set(()).is_err() {
            return;
        }
        // Keep only a weak App Server reference. Capturing a strong reference
        // would prevent runtime shutdown from reclaiming the child process.
        let app_server = Arc::downgrade(&self.inner.app_server);
        let router = self.inner.router.clone();
        tokio::spawn(async move {
            let Some(app_server) = app_server.upgrade() else {
                return;
            };
            let mut events = app_server.subscribe();
            drop(app_server);
            loop {
                match events.recv().await {
                    Ok(event) => router.handle_server_event(event),
                    Err(broadcast::error::RecvError::Closed) => {
                        router.close_all();
                        return;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // A lagged receiver cannot reconstruct the missing
                        // prefix. Disconnect every session rather than expose
                        // a partial event stream.
                        router.close_all();
                        return;
                    }
                }
            }
        });
    }
}

fn is_daemon_lifecycle(method: &str) -> bool {
    matches!(method, "initialize" | "initialized")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestRoute<'a> {
    DaemonLifecycle,
    DesktopProjectList,
    Upstream {
        method: &'a str,
        enrich_threads: bool,
    },
}

fn classify_request(method: &str) -> RequestRoute<'_> {
    if is_daemon_lifecycle(method) {
        return RequestRoute::DaemonLifecycle;
    }
    if method == HOST_PROJECT_LIST_METHOD {
        return RequestRoute::DesktopProjectList;
    }
    let (method, enrich_threads) = match method {
        HOST_THREAD_LIST_METHOD => ("thread/list", true),
        HOST_THREAD_READ_METHOD => ("thread/read", true),
        HOST_THREAD_START_METHOD => ("thread/start", true),
        method => (method, false),
    };
    RequestRoute::Upstream {
        method,
        enrich_threads,
    }
}

fn daemon_owned_error(method: &str) -> RpcError {
    RpcError {
        code: json!("daemon_owned_method"),
        message: format!("{method} is handled by the Host daemon"),
        data: None,
        extensions: Map::new(),
    }
}

fn desktop_project_failure(error: DesktopProjectError) -> RpcOutcome {
    RpcOutcome::Failure {
        error: RpcError {
            code: json!("desktop_project_state_unavailable"),
            message: error.to_string(),
            data: None,
            extensions: Map::new(),
        },
    }
}

fn daemon_owned_response(id: RpcId, method: &str, extensions: Map<String, Value>) -> RpcResponse {
    RpcResponse {
        id,
        outcome: RpcOutcome::Failure {
            error: daemon_owned_error(method),
        },
        extensions,
    }
}

fn app_server_response(
    response: Result<RawResponse, AppServerError>,
) -> (RpcOutcome, Map<String, Value>) {
    match response {
        Ok(response) => (
            RpcOutcome::Success {
                result: response.result,
            },
            response.extensions,
        ),
        Err(AppServerError::Remote { detail, .. }) => {
            let RemoteError {
                code,
                message,
                data,
                additional_fields,
                response_extensions,
            } = *detail;
            (
                RpcOutcome::Failure {
                    error: RpcError {
                        code,
                        message,
                        data,
                        extensions: additional_fields,
                    },
                },
                response_extensions,
            )
        }
        Err(error) => (
            RpcOutcome::Failure {
                error: RpcError {
                    code: json!("codex_unavailable"),
                    message: error.to_string(),
                    data: None,
                    extensions: Map::new(),
                },
            },
            Map::new(),
        ),
    }
}

fn server_response(
    response: &RpcResponse,
) -> Result<(ServerResponse, Map<String, Value>), DispatchError> {
    match &response.outcome {
        RpcOutcome::Success { result } => Ok((
            ServerResponse::Result {
                result: result.clone(),
            },
            response.extensions.clone(),
        )),
        RpcOutcome::Failure { error } => Ok((
            ServerResponse::Error {
                error: serde_json::to_value(error)
                    .map_err(|error| DispatchError::Serialization(error.to_string()))?,
            },
            response.extensions.clone(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_classifier_covers_daemon_host_aliases_and_passthrough() {
        let cases = [
            ("initialize", RequestRoute::DaemonLifecycle),
            ("initialized", RequestRoute::DaemonLifecycle),
            (HOST_PROJECT_LIST_METHOD, RequestRoute::DesktopProjectList),
            (
                HOST_THREAD_LIST_METHOD,
                RequestRoute::Upstream {
                    method: "thread/list",
                    enrich_threads: true,
                },
            ),
            (
                HOST_THREAD_READ_METHOD,
                RequestRoute::Upstream {
                    method: "thread/read",
                    enrich_threads: true,
                },
            ),
            (
                HOST_THREAD_START_METHOD,
                RequestRoute::Upstream {
                    method: "thread/start",
                    enrich_threads: true,
                },
            ),
            (
                "thread/list",
                RequestRoute::Upstream {
                    method: "thread/list",
                    enrich_threads: false,
                },
            ),
            (
                "future/method",
                RequestRoute::Upstream {
                    method: "future/method",
                    enrich_threads: false,
                },
            ),
        ];

        for (method, expected) in cases {
            assert_eq!(classify_request(method), expected, "method: {method}");
        }
    }

    #[test]
    fn desktop_project_errors_use_a_stable_host_error_code() {
        let RpcOutcome::Failure { error } =
            desktop_project_failure(DesktopProjectError::InvalidCursor)
        else {
            panic!("expected failure")
        };
        assert_eq!(error.code, json!("desktop_project_state_unavailable"));
    }

    #[test]
    fn daemon_owned_response_preserves_top_level_extensions() {
        let response = daemon_owned_response(
            RpcId::String("mobile-request-1".to_owned()),
            "initialize",
            Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        );

        assert_eq!(response.extensions.get("jsonrpc"), Some(&json!("2.0")));
        assert!(matches!(response.outcome, RpcOutcome::Failure { .. }));
    }

    #[test]
    fn server_response_preserves_result_and_structured_error() {
        let response = RpcResponse {
            id: RpcId::Integer(4),
            outcome: RpcOutcome::Failure {
                error: RpcError {
                    code: json!(-32000),
                    message: "nope".to_owned(),
                    data: Some(json!({"retryable": true})),
                    extensions: Map::from_iter([(String::from("vendor"), json!(true))]),
                },
            },
            extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        };
        assert_eq!(
            server_response(&response).unwrap(),
            (
                ServerResponse::Error {
                    error: json!({
                        "code": -32000,
                        "message": "nope",
                        "data": {"retryable": true},
                        "vendor": true
                    })
                },
                Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
            )
        );
    }

    #[test]
    fn app_server_success_uses_upstream_response_extensions() {
        let (outcome, extensions) = app_server_response(Ok(RawResponse {
            result: json!({"accepted": true, "futureResult": [1, 2, 3]}),
            extensions: Map::from_iter([
                (String::from("jsonrpc"), json!("2.0")),
                (String::from("upstreamFuture"), json!({"kept": true})),
            ]),
        }));
        assert_eq!(
            outcome,
            RpcOutcome::Success {
                result: json!({"accepted": true, "futureResult": [1, 2, 3]}),
            }
        );
        assert_eq!(extensions.get("jsonrpc"), Some(&json!("2.0")));
        assert_eq!(
            extensions.get("upstreamFuture"),
            Some(&json!({"kept": true}))
        );
    }

    #[test]
    fn app_server_error_keeps_error_and_response_extension_namespaces_separate() {
        let (outcome, extensions) = app_server_response(Err(AppServerError::Remote {
            method: "future/method".to_owned(),
            detail: Box::new(RemoteError {
                code: json!(-32001),
                message: "approval unavailable".to_owned(),
                data: Some(json!({"retryable": true})),
                additional_fields: Map::from_iter([(
                    String::from("errorFuture"),
                    json!([1, 2, 3]),
                )]),
                response_extensions: Map::from_iter([
                    (String::from("jsonrpc"), json!("2.0")),
                    (String::from("upstreamFuture"), json!({"kept": true})),
                ]),
            }),
        }));

        assert_eq!(
            outcome,
            RpcOutcome::Failure {
                error: RpcError {
                    code: json!(-32001),
                    message: "approval unavailable".to_owned(),
                    data: Some(json!({"retryable": true})),
                    extensions: Map::from_iter([(String::from("errorFuture"), json!([1, 2, 3]),)]),
                },
            }
        );
        assert_eq!(extensions.get("jsonrpc"), Some(&json!("2.0")));
        assert_eq!(
            extensions.get("upstreamFuture"),
            Some(&json!({"kept": true}))
        );
    }
}
