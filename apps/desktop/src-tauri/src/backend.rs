use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use codex_app_server::{AppServerConfig, ClientInfo, CodexAppServer};
use host_daemon::{
    DesktopProjectStore, HOST_PROJECT_LIST_METHOD, HOST_THREAD_LIST_METHOD,
    HOST_THREAD_READ_METHOD, HOST_THREAD_START_METHOD,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct DesktopBackend {
    app_server: CodexAppServer,
    projects: DesktopProjectStore,
    next_request_id: AtomicU64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionInfo {
    user_agent: String,
    platform_family: String,
    platform_os: String,
    codex_home: String,
}

impl DesktopBackend {
    pub(crate) async fn spawn() -> Result<Self, String> {
        let app_server = CodexAppServer::spawn(AppServerConfig {
            client: ClientInfo {
                name: "remote_agent_desktop".to_owned(),
                title: "Remote Agent Desktop".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            request_timeout: REQUEST_TIMEOUT,
            ..AppServerConfig::default()
        })
        .await
        .map_err(|error| error.to_string())?;
        let projects =
            DesktopProjectStore::from_environment().map_err(|error| error.to_string())?;

        Ok(Self {
            app_server,
            projects,
            next_request_id: AtomicU64::new(0),
        })
    }

    pub(crate) fn connection_info(&self) -> ConnectionInfo {
        let response = self.app_server.initialize_response();
        ConnectionInfo {
            user_agent: response.user_agent.clone(),
            platform_family: response.platform_family.clone(),
            platform_os: response.platform_os.clone(),
            codex_home: response.codex_home.to_string_lossy().into_owned(),
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<String> {
        self.app_server.subscribe()
    }

    pub(crate) async fn request(&self, method: String, params: Value) -> Result<Value, String> {
        let id = self.next_request_id()?;
        if method == HOST_PROJECT_LIST_METHOD {
            return self
                .projects
                .project_list(&params)
                .await
                .map_err(|error| error.to_string());
        }

        let upstream_method = desktop_upstream_method(&method);
        let line = serde_json::to_string(&json!({
            "id": id,
            "method": upstream_method.unwrap_or(&method),
            "params": params,
        }))
        .map_err(|error| error.to_string())?;
        let response = self
            .app_server
            .request_raw(&line)
            .await
            .map_err(|error| error.to_string())?;
        let result = response_result(&response)?;

        if upstream_method.is_some() {
            return self
                .projects
                .enrich_threads(result)
                .await
                .map_err(|error| error.to_string());
        }
        Ok(result)
    }

    pub(crate) async fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        self.send_response(id, "result", result).await
    }

    pub(crate) async fn respond_error(&self, id: Value, error: Value) -> Result<(), String> {
        self.send_response(id, "error", error).await
    }

    fn next_request_id(&self) -> Result<u64, String> {
        let previous_id = self
            .next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| "request id space is exhausted".to_owned())?;
        Ok(previous_id + 1)
    }

    async fn send_response(&self, id: Value, field: &str, payload: Value) -> Result<(), String> {
        let mut response = Map::from_iter([("id".to_owned(), id)]);
        response.insert(field.to_owned(), payload);
        let line = serde_json::to_string(&response).map_err(|error| error.to_string())?;
        self.app_server
            .send_raw(&line)
            .await
            .map_err(|error| error.to_string())
    }
}

fn desktop_upstream_method(method: &str) -> Option<&'static str> {
    match method {
        HOST_THREAD_LIST_METHOD => Some("thread/list"),
        HOST_THREAD_READ_METHOD => Some("thread/read"),
        HOST_THREAD_START_METHOD => Some("thread/start"),
        _ => None,
    }
}

fn response_result(line: &str) -> Result<Value, String> {
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

    #[test]
    fn maps_only_desktop_thread_methods() {
        assert_eq!(
            desktop_upstream_method(HOST_THREAD_LIST_METHOD),
            Some("thread/list")
        );
        assert_eq!(
            desktop_upstream_method(HOST_THREAD_READ_METHOD),
            Some("thread/read")
        );
        assert_eq!(
            desktop_upstream_method(HOST_THREAD_START_METHOD),
            Some("thread/start")
        );
        assert_eq!(desktop_upstream_method("turn/start"), None);
    }

    #[test]
    fn extracts_result_and_remote_error() {
        assert_eq!(
            response_result(r#"{"id":1,"result":{"value":7}}"#).unwrap(),
            json!({"value": 7})
        );
        assert_eq!(
            response_result(r#"{"id":1,"error":{"code":-1,"message":"nope"}}"#).unwrap_err(),
            "nope"
        );
    }
}
