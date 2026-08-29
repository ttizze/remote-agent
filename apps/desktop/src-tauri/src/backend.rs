use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use codex_app_server::{AppServerConfig, ClientInfo, CodexAppServer};
use host_daemon::DesktopProjectStore;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::backend_rpc::{self, RequestRoute, ResponseKind};

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
        let route = RequestRoute::for_method(&method);
        if route.is_project_list() {
            return self
                .projects
                .project_list(&params)
                .await
                .map_err(|error| error.to_string());
        }

        let line = backend_rpc::request_line(id, route.upstream_method(&method), &params)?;
        let response = self
            .app_server
            .request_raw(&line)
            .await
            .map_err(|error| error.to_string())?;
        let result = backend_rpc::response_result(&response)?;

        if route.enriches_threads() {
            return self
                .projects
                .enrich_threads(result)
                .await
                .map_err(|error| error.to_string());
        }
        Ok(result)
    }

    pub(crate) async fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        self.send_response(id, ResponseKind::Result, result).await
    }

    pub(crate) async fn respond_error(&self, id: Value, error: Value) -> Result<(), String> {
        self.send_response(id, ResponseKind::Error, error).await
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

    async fn send_response(
        &self,
        id: Value,
        kind: ResponseKind,
        payload: Value,
    ) -> Result<(), String> {
        let line = backend_rpc::response_line(id, kind, payload)?;
        self.app_server
            .send_raw(&line)
            .await
            .map_err(|error| error.to_string())
    }
}
