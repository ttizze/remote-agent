//! Provider account, catalog and configuration operations, without conversation state.
use super::{identity::Identity, service::Failure};
use agent_protocol::{
    models::{Model, ReasoningEffort},
    operations as op,
    session::ProviderKind,
};
use codex_app_server::CodexAppServer;
use serde::Serialize;
use serde_json::Value;
use std::{path::PathBuf, sync::Arc};
pub(super) struct CodexResources {
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    process: Result<Arc<CodexAppServer>, String>,
    pub directory: PathBuf,
}
impl CodexResources {
    pub fn new(process: Result<Arc<CodexAppServer>, String>) -> Self {
        let directory = process
            .as_ref()
            .ok()
            .map(|server| server.initialize_response().codex_home.clone())
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| {
                directories::BaseDirs::new()
                    .map(|dirs| dirs.home_dir().join(".codex"))
                    .unwrap_or_default()
            });
        Self {
            accounts: Default::default(),
            restoration_error: tokio::sync::watch::channel(None).0,
            process,
            directory,
        }
    }
    pub fn server(&self) -> Result<&CodexAppServer, Failure> {
        self.process
            .as_deref()
            .map_err(|error| Failure::new("provider_unavailable", error))
    }
    pub fn availability(&self) -> Result<(), Failure> {
        self.server()?;
        if let Some(error) = self.restoration_error.borrow().clone() {
            return Err(Failure::new("account_unavailable", error));
        }
        Ok(())
    }
    pub(super) async fn request<P: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<T, Failure> {
        self.server()?
            .request(method, params)
            .await
            .map_err(|error| Failure::new("provider_failed", error))?
            .outcome
            .map_err(|error| Failure::new("provider_failed", error.get()))
    }
    pub fn auth_requests(self: &Arc<Self>) -> Option<tokio_util::task::AbortOnDropHandle<()>> {
        let process = self.process.as_ref().ok()?.clone();
        let mut events = process.subscribe();
        let resources = self.clone();
        Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
            async move {
                while let Ok(event) = events.recv().await {
                    let agent_transport::peer::PeerEvent::Message(message) = event else {
                        continue;
                    };
                    let Ok(request) = agent_transport::peer::RpcMessage::parse(&message.value)
                    else {
                        continue;
                    };
                    if request.kind() != agent_transport::peer::RpcMessageKind::Request
                        || request.method() != Some("account/chatgptAuthTokens/refresh")
                    {
                        continue;
                    }
                    #[derive(serde::Deserialize)]
                    #[serde(rename_all = "camelCase")]
                    struct Refresh {
                        previous_account_id: Option<String>,
                    }
                    let Ok(params) = request.params::<Refresh>() else {
                        continue;
                    };
                    let response = {
                        let mut accounts = resources.accounts.lock().await;
                        match accounts.as_mut() {
                            Some(accounts) => match accounts
                                .refresh(params.previous_account_id.as_deref())
                                .await
                            {
                                Ok(credentials) => request.response::<_, ()>(Ok(credentials)),
                                Err(error) => request.error(-32000, &error),
                            },
                            None => request
                                .error(-32000, &"Select an account before refreshing credentials"),
                        }
                    };
                    if let Ok(response) = response {
                        let line = zeroize::Zeroizing::new(response);
                        let _ = process.send_raw(&line).await;
                    }
                }
            },
        )))
    }
    pub(super) async fn enable_accounts(
        &self,
        directory: PathBuf,
        config: codex_app_server::AppServerConfig,
    ) -> Result<(), String> {
        let accounts = crate::codex_accounts::Accounts::load(
            directory,
            config,
            self.server().map_err(|error| error.to_string())?,
            self.restoration_error.clone(),
        )
        .await
        .inspect_err(|error| {
            self.restoration_error.send_replace(Some(error.clone()));
        })?;
        *self.accounts.lock().await = Some(accounts);
        Ok(())
    }
    pub(super) async fn models(&self, params: &op::ListModels) -> Result<op::ModelPage, Failure> {
        let mut native: Value = self.request("model/list", params).await?;
        let data = native["data"]
            .as_array_mut()
            .ok_or_else(|| Failure::new("invalid_models", "native model catalog is missing"))?;
        for model in data {
            let id = model["model"].take();
            model["model"] = serde_json::json!({"provider":"codex","id":id});
        }
        serde_json::from_value(native).map_err(Into::into)
    }
}
#[async_trait::async_trait]
impl Identity for CodexResources {
    async fn list(&self) -> Result<op::Accounts, Failure> {
        let mut accounts = self.accounts.lock().await;
        let accounts = accounts.as_mut().ok_or_else(|| {
            Failure::new("account_unavailable", "アカウント管理が利用できません。")
        })?;
        self.server()?;
        accounts
            .list()
            .await
            .map_err(|e| Failure::new("account_operation_failed", e))
    }
    async fn account(
        &self,
        command: super::identity::AccountCommand,
    ) -> Result<super::identity::AccountReply, Failure> {
        let mut accounts = self.accounts.lock().await;
        accounts
            .as_mut()
            .ok_or_else(|| Failure::new("account_unavailable", "アカウント管理が利用できません。"))?
            .request(self.server()?, command)
            .await
            .map_err(|e| Failure::new("account_operation_failed", e))
    }
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, Failure> {
        let fetch = self
            .accounts
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| Failure::new("account_unavailable", "アカウント管理が利用できません。"))?
            .usage_request(id)
            .map_err(|e| Failure::new("account_operation_failed", e))?;
        Ok(fetch.await)
    }
}

pub(super) struct ClaudeResources {
    accounts: tokio::sync::Mutex<crate::claude::accounts::Accounts>,
    pub adapter: Arc<provider_adapters::claude::ClaudeAdapter>,
    pub native_home: PathBuf,
    directory: PathBuf,
    config: provider_adapters::claude::ClaudeConfig,
}
impl ClaudeResources {
    pub async fn load(
        program: PathBuf,
        directory: PathBuf,
        native_home: Option<PathBuf>,
        output: tokio::sync::mpsc::Sender<provider_adapters::ProviderBatch>,
    ) -> anyhow::Result<Self> {
        crate::platform::create_state_directory(&directory)?;
        let native_home = native_home
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".claude")))
            .ok_or_else(|| anyhow::anyhow!("Claude home unavailable"))?;
        let accounts = crate::claude::accounts::Accounts::load(
            program.clone(),
            directory.join("accounts"),
            native_home.clone(),
        )
        .await?;
        let config = provider_adapters::claude::ClaudeConfig {
            program,
            config_home: native_home.clone(),
        };
        let adapter = Arc::new(provider_adapters::claude::ClaudeAdapter::new(
            config.clone(),
            output,
        ));
        Ok(Self {
            accounts: tokio::sync::Mutex::new(accounts),
            adapter,
            native_home,
            directory,
            config,
        })
    }
    pub async fn credentials_home(&self) -> Result<PathBuf, Failure> {
        self.accounts
            .lock()
            .await
            .selected_home()
            .map_err(|error| Failure::new("account_unavailable", error))?
            .ok_or_else(|| Failure::new("account_unavailable", "select a Claude account"))
    }
    pub async fn models(&self) -> Result<Vec<Model>, String> {
        let auth_home = self
            .credentials_home()
            .await
            .map_err(|error| error.to_string())?;
        let cwd = tempfile::tempdir_in(&self.directory).map_err(|error| error.to_string())?;
        let initialized =
            provider_adapters::claude::query_control(&self.config, &auth_home, cwd.path(), None)
                .await
                .map_err(|error| error.to_string())?;
        let entries = initialized["models"]
            .as_array()
            .ok_or("Claude Code did not return a model catalog")?;
        entries
            .iter()
            .map(|entry| {
                let name = entry["value"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .ok_or("Claude model has no value")?;
                let display = entry["displayName"]
                    .as_str()
                    .ok_or("Claude model has no display name")?;
                // The CLI's short display name omits the model generation.
                // Its description starts with the versioned name and context size.
                let title = entry["description"]
                    .as_str()
                    .and_then(|description| description.split('·').next())
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .unwrap_or(display);
                let display = if name == "default" && title != display {
                    format!("{display} · {title}")
                } else {
                    title.to_owned()
                };
                let values = match entry.get("supportedEffortLevels") {
                    Some(value) => value
                        .as_array()
                        .ok_or("invalid Claude effort levels")?
                        .as_slice(),
                    None => &[],
                };
                let efforts = values
                    .iter()
                    .map(|value| {
                        Ok(ReasoningEffort {
                            reasoning_effort: value
                                .as_str()
                                .ok_or("invalid Claude effort level")?
                                .into(),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let default = efforts
                    .iter()
                    .find(|effort| effort.reasoning_effort == "high")
                    .or(efforts.first())
                    .map(|effort| effort.reasoning_effort.clone())
                    .unwrap_or_default();
                let model = agent_protocol::models::ModelRef {
                    provider: ProviderKind::Claude,
                    id: name.into(),
                };
                Ok(Model {
                    id: name.into(),
                    model,
                    display_name: format!("Claude · {display}"),
                    default_reasoning_effort: default,
                    supported_reasoning_efforts: efforts,
                    service_tiers: Some(Vec::new()),
                    default_service_tier: None,
                    is_default: Some(false),
                })
            })
            .collect::<Result<Vec<Model>, String>>()
    }
}
#[async_trait::async_trait]
impl crate::host_rpc::identity::Identity for ClaudeResources {
    async fn list(&self) -> Result<op::Accounts, crate::host_rpc::service::Failure> {
        let (accounts, selected) =
            self.accounts.lock().await.list().await.map_err(|e| {
                crate::host_rpc::service::Failure::new("account_operation_failed", e)
            })?;
        Ok(op::Accounts {
            accounts,
            selected: selected
                .map(|id| (ProviderKind::Claude, id))
                .into_iter()
                .collect(),
            error: None,
        })
    }
    async fn account(
        &self,
        command: crate::host_rpc::identity::AccountCommand,
    ) -> Result<crate::host_rpc::identity::AccountReply, crate::host_rpc::service::Failure> {
        self.accounts
            .lock()
            .await
            .request(command)
            .await
            .map_err(|e| crate::host_rpc::service::Failure::new("account_operation_failed", e))
    }
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, crate::host_rpc::service::Failure> {
        let fetch =
            self.accounts.lock().await.usage_request(id).map_err(|e| {
                crate::host_rpc::service::Failure::new("account_operation_failed", e)
            })?;
        Ok(fetch.await)
    }
}
