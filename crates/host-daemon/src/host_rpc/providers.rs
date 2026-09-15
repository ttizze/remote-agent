//! The Host-facing provider boundary. Backend IDs and wire behavior stay here;
//! workspace preparation and project membership remain owned by the Host.
mod catalog;
mod claude;
mod codex;
use super::{
    Failure, invalid_message,
    routing::{SessionId, SessionRouter, UpstreamRequest},
};
use agent_core::{
    models::{ThreadParams, ThreadResponse},
    peer::{RpcMessage, RpcMessageError, RpcResponse},
};
pub(super) use catalog::ThreadCatalog;
use codex_app_server::CodexAppServer;
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Provider {
    Codex,
    Claude,
}
impl Provider {
    fn from_id(id: Option<&str>) -> Self {
        if id.is_some_and(|id| id.starts_with(claude::MODEL_PREFIX)) {
            Self::Claude
        } else {
            Self::Codex
        }
    }
}
pub(super) enum ProviderResponse {
    Raw(String),
    Thread(Box<RpcResponse<ThreadResponse>>),
}
pub(super) struct Providers {
    codex: codex::Codex,
    claude: OnceLock<claude::Claude>,
    router: SessionRouter,
}
impl Providers {
    pub(super) fn new(codex: Result<Arc<CodexAppServer>, String>, router: SessionRouter) -> Self {
        Self {
            codex: codex::Codex::new(codex, router.clone()),
            claude: OnceLock::new(),
            router,
        }
    }
    pub(super) async fn enable_accounts(
        &self,
        directory: std::path::PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        self.codex.enable_accounts(directory, config).await
    }
    pub(super) async fn enable_claude(
        &self,
        program: std::path::PathBuf,
        directory: std::path::PathBuf,
    ) -> Result<(), String> {
        let claude = claude::Claude::load(program, directory, self.router.clone()).await?;
        self.claude
            .set(claude)
            .map_err(|_| "Claude Code is already configured".into())
    }
    pub(super) async fn shutdown_claude(&self) {
        if let Some(claude) = self.claude.get() {
            claude.shutdown().await;
        }
    }
    pub(super) fn start(&self) {
        self.codex.start();
    }
    pub(super) fn close_session(&self, session: SessionId) {
        self.codex.close_session(session);
    }
    pub(super) fn errors(&self) -> serde_json::Value {
        match self.codex.codex() {
            Ok(_) => serde_json::json!({}),
            Err(error) => serde_json::json!({"codex":error}),
        }
    }
    pub(super) fn process_directories(&self) -> Vec<std::path::PathBuf> {
        self.codex.process_directories()
    }
    pub(super) async fn respond(
        &self,
        upstream: UpstreamRequest,
        response: &RpcMessage<'_>,
    ) -> Result<(), String> {
        let line = response.rewrite_id(&upstream.id).map_err(invalid_message)?;
        match upstream.provider {
            Provider::Codex => self.codex.send(&line).await,
            Provider::Claude => {
                if let Some(claude) = self.claude.get() {
                    claude
                        .respond(&RpcMessage::parse(&line).map_err(invalid_message)?)
                        .await?;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn notify(&self, line: &str) {
        self.codex.send(line).await;
    }
    pub(super) fn check_start(&self, model: Option<&str>) -> Result<(), Failure> {
        if Provider::from_id(model) == Provider::Codex {
            self.codex.codex()?;
        }
        Ok(())
    }
    pub(super) async fn start_thread(
        &self,
        request: &RpcMessage<'_>,
        params: ThreadParams,
    ) -> Result<RpcResponse<ThreadResponse>, Failure> {
        let model = params
            .extra
            .get("model")
            .and_then(serde_json::Value::as_str);
        match Provider::from_id(model) {
            Provider::Codex => {
                self.codex
                    .thread_request(request, "thread/start", params, false)
                    .await
            }
            Provider::Claude => {
                let claude = self.claude.get().ok_or_else(|| {
                    Failure::new(
                        "claude_unavailable",
                        "このHostではClaude Codeが有効になっていません。",
                    )
                })?;
                let response = claude
                    .create(
                        params.cwd.as_deref().unwrap_or_default(),
                        model.expect("Claude model selected"),
                    )
                    .await
                    .map_err(|error| Failure::new("claude_unavailable", error))?;
                Ok(RpcResponse::parse(
                    &request.response::<_, Failure>(Ok(response))?,
                )?)
            }
        }
    }
    pub(super) async fn request(
        &self,
        session: SessionId,
        request: &RpcMessage<'_>,
    ) -> Result<ProviderResponse, RpcMessageError> {
        #[derive(Default, serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Target<'a> {
            thread_id: Option<&'a str>,
            model: Option<&'a str>,
        }
        let target = request.params::<Target>().unwrap_or_default();
        let method = request.method().expect("classified request has a method");
        if Provider::from_id(target.thread_id) == Provider::Claude
            && !matches!(method, "host/thread/watch" | "host/thread/unwatch")
        {
            let result = match self.claude.get() {
                Some(claude) => claude
                    .request(method, request.params()?)
                    .await
                    .map_err(|error| Failure::new("claude_failed", error)),
                None => Err(Failure::new(
                    "claude_unavailable",
                    "このHostではClaude Codeが有効になっていません。",
                )),
            };
            let enrich = result
                .as_ref()
                .is_ok_and(|value| value.get("thread").is_some());
            let response = request.response(result)?;
            return if enrich {
                Ok(ProviderResponse::Thread(Box::new(RpcResponse::parse(
                    &response,
                )?)))
            } else {
                Ok(ProviderResponse::Raw(response))
            };
        }
        if method == "turn/start" && Provider::from_id(target.model) == Provider::Claude {
            return Ok(ProviderResponse::Raw(request.error(
                "provider_mismatch",
                &"Claudeへ切り替える場合は新しい会話を作成してください。",
            )?));
        }
        if method == "model/list" && self.claude.get().is_some() {
            return self.model_list(request).await.map(ProviderResponse::Raw);
        }
        if method == "host/thread/read" {
            return match self
                .codex
                .thread_request(request, "thread/read", request.params()?, true)
                .await
            {
                Ok(response) => Ok(ProviderResponse::Thread(Box::new(response))),
                Err(error) => Ok(ProviderResponse::Raw(
                    request.response::<(), _>(Err(error))?,
                )),
            };
        }
        self.codex
            .request(session, request)
            .await
            .map(ProviderResponse::Raw)
    }
    async fn model_list(&self, request: &RpcMessage<'_>) -> Result<String, RpcMessageError> {
        let line = match self.codex.codex_request(request.line()).await {
            Ok(line) => line,
            Err(error) => request.response::<agent_core::client::ModelPage, _>(Err(error))?,
        };
        let mut response = RpcResponse::<agent_core::client::ModelPage>::parse(&line)?;
        if let Err(error) = &response.outcome {
            let mut extra = serde_json::Map::new();
            extra.insert("providerErrors".into(), serde_json::json!({"codex":error}));
            response.outcome = Ok(agent_core::client::ModelPage {
                data: Vec::new(),
                next_cursor: None,
                extra,
            });
        }
        let first_page = request.params::<serde_json::Value>()?["cursor"].is_null();
        let page = response.outcome.as_mut().expect("model page initialized");
        if first_page {
            match self.claude.get().unwrap().models().await {
                Ok(models) => page.data.extend_from_slice(models),
                Err(error) => {
                    let errors = page
                        .extra
                        .entry("providerErrors")
                        .or_insert_with(|| serde_json::json!({}));
                    let Some(errors) = errors.as_object_mut() else {
                        return request
                            .error("invalid_model_catalog", &"providerErrors must be an object");
                    };
                    errors.insert("claude".into(), serde_json::json!({"message":error}));
                }
            }
        }
        if first_page && page.data.is_empty() && page.extra.contains_key("providerErrors") {
            request.error("models_unavailable", &page.extra["providerErrors"])
        } else {
            Ok(serde_json::to_string(&response)?)
        }
    }
}
