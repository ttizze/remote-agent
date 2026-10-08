//! Provider account, catalog and configuration operations, without conversation state.
use super::{identity::Identity, service::Failure};
use agent_protocol::{models::Model, operations as op, provider::ProviderKind};
use codex_app_server::CodexAppServer;
use serde::Serialize;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub(super) struct CodexResources {
    accounts: tokio::sync::Mutex<Option<crate::codex_accounts::Accounts>>,
    restoration_error: tokio::sync::watch::Sender<Option<String>>,
    process: Result<Arc<CodexAppServer>, String>,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct CpuCounters {
    total: u64,
    idle: u64,
    count: u64,
    at: Instant,
}

static CPU_COUNTERS: OnceLock<Mutex<Option<CpuCounters>>> = OnceLock::new();

fn linux_cpu_counters() -> Option<(u64, u64, u64)> {
    let line = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    let total = values.iter().copied().sum();
    let idle =
        values.get(3).copied().unwrap_or_default() + values.get(4).copied().unwrap_or_default();
    let count = std::thread::available_parallelism().ok()?.get() as u64;
    (total > 0 && count > 0).then_some((total, idle, count))
}

fn linux_memory() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let value = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kb| kb.saturating_mul(1024))
    };
    Some((value("MemAvailable:")?, value("MemTotal:")?))
}

#[cfg(target_os = "macos")]
fn mac_cpu_utilization() -> Option<f64> {
    let count = std::thread::available_parallelism().ok()?.get() as f64;
    let mut load = 0.0;
    // `getloadavg` is the only process-free whole-host CPU signal available
    // through the standard macOS libc surface. A one-minute load normalized by
    // logical CPUs is a conservative routing signal.
    let result = unsafe { libc::getloadavg(&mut load, 1) };
    (result == 1).then_some((load / count).clamp(0.0, 1.0))
}

#[cfg(target_os = "macos")]
fn mac_memory() -> Option<(u64, u64)> {
    let pagesize = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let physical = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let available = unsafe { libc::sysconf(libc::_SC_AVPHYS_PAGES) };
    (pagesize > 0 && physical > 0 && available > 0).then_some((
        (available as u64).saturating_mul(pagesize as u64),
        (physical as u64).saturating_mul(pagesize as u64),
    ))
}

pub(super) async fn host_resources() -> agent_protocol::models::HostResourcesSnapshot {
    tokio::task::spawn_blocking(|| {
        let sampled_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        #[cfg(target_os = "linux")]
        let cpu_utilization = linux_cpu_counters().and_then(|(total, idle, count)| {
            let now = Instant::now();
            let state = CPU_COUNTERS.get_or_init(|| Mutex::new(None));
            let mut previous = state.lock().ok()?;
            let current = CpuCounters {
                total,
                idle,
                count,
                at: now,
            };
            let utilization = previous.as_ref().and_then(|old| {
                let total_delta = total.saturating_sub(old.total);
                let idle_delta = idle.saturating_sub(old.idle);
                (old.count == count
                    && total_delta > 0
                    && now.duration_since(old.at) >= Duration::from_millis(20))
                .then(|| (1.0 - idle_delta as f64 / total_delta as f64).clamp(0.0, 1.0))
            });
            *previous = Some(current);
            utilization
        });
        #[cfg(target_os = "macos")]
        let cpu_utilization = mac_cpu_utilization();
        let cpu_count = std::thread::available_parallelism()
            .map(|count| count.get() as u64)
            .unwrap_or_default();
        #[cfg(target_os = "linux")]
        let memory = linux_memory();
        #[cfg(target_os = "macos")]
        let memory = mac_memory();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let memory = None;
        let (available_memory_bytes, total_memory_bytes) = memory.unwrap_or((0, 0));
        agent_protocol::models::HostResourcesSnapshot {
            sampled_at,
            cpu_utilization,
            cpu_count,
            available_memory_bytes,
            total_memory_bytes,
        }
    })
    .await
    .unwrap_or(agent_protocol::models::HostResourcesSnapshot {
        sampled_at: 0,
        cpu_utilization: None,
        cpu_count: 0,
        available_memory_bytes: 0,
        total_memory_bytes: 0,
    })
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
    /// No account is selected after a sign-out.
    pub fn signed_out(&self) -> bool {
        self.restoration_error.borrow().is_some()
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
    /// Every page of the app-server's `model/list`.
    pub(super) async fn models(&self) -> Result<Vec<Model>, Failure> {
        let mut native = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let page: Value = self
                .request(
                    "model/list",
                    &serde_json::json!({"limit": 100, "cursor": cursor}),
                )
                .await?;
            native.extend(
                page["data"]
                    .as_array()
                    .ok_or_else(|| {
                        Failure::new("invalid_models", "native model catalog is missing")
                    })?
                    .iter()
                    .cloned(),
            );
            let next = page["nextCursor"].as_str().map(str::to_owned);
            if next.is_none() {
                break;
            }
            if next == cursor {
                return Err(Failure::new(
                    "invalid_models",
                    "Model catalog repeated its cursor",
                ));
            }
            cursor = next;
        }
        let shares_tokens = crate::conversation::CodexCredentials::shares_tokens(self).await;
        Ok(agent_providers::codex_catalog(&native, shares_tokens)
            .map_err(|error| Failure::new("invalid_models", error))?
            .into_iter()
            .map(wire_model)
            .collect())
    }
}

pub(super) fn wire_model(model: agent_providers::CatalogModel) -> Model {
    Model {
        slug: model.slug,
        name: model.name,
        aliases: model.aliases,
        badge: model.badge,
        is_default: model.is_default,
        is_legacy: model.is_legacy,
        option_descriptors: model.descriptors,
    }
}
impl crate::conversation::CodexCredentials for CodexResources {
    fn login(&self) -> futures_util::future::BoxFuture<'_, Result<Option<Value>, String>> {
        Box::pin(async move {
            let mut accounts = self.accounts.lock().await;
            match accounts.as_mut() {
                Some(accounts) => accounts.session_login().await,
                None => Ok(None),
            }
        })
    }
    fn shares_tokens(&self) -> futures_util::future::BoxFuture<'_, bool> {
        Box::pin(async move {
            self.accounts
                .lock()
                .await
                .as_ref()
                .is_some_and(|accounts| accounts.shares_tokens())
        })
    }
    fn refresh(
        &self,
        previous_account: Option<String>,
    ) -> futures_util::future::BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            let mut accounts = self.accounts.lock().await;
            let accounts = accounts
                .as_mut()
                .ok_or("Select an account before refreshing credentials")?;
            let credentials = accounts.refresh(previous_account.as_deref()).await?;
            serde_json::to_value(credentials).map_err(|error| error.to_string())
        })
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

pub(crate) struct ClaudeResources {
    accounts: tokio::sync::Mutex<crate::claude::accounts::Accounts>,
    pub native_home: PathBuf,
    program: PathBuf,
}
impl ClaudeResources {
    pub async fn load(
        program: PathBuf,
        directory: PathBuf,
        native_home: Option<PathBuf>,
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
        Ok(Self {
            accounts: tokio::sync::Mutex::new(accounts),
            native_home,
            program,
        })
    }
    pub(crate) fn program(&self) -> crate::claude::control::ClaudeProgram {
        crate::claude::control::ClaudeProgram {
            program: self.program.clone(),
            config_home: self.native_home.clone(),
            environment: Default::default(),
            launch_args: vec![],
        }
    }
    pub async fn credentials_home(&self) -> Result<PathBuf, Failure> {
        self.accounts
            .lock()
            .await
            .selected_home()
            .map_err(|error| Failure::new("account_unavailable", error))?
            .ok_or_else(|| Failure::new("account_unavailable", "select a Claude account"))
    }
    /// The installed Claude Code's version from `--version`.
    pub async fn version(&self) -> Option<String> {
        let mut command = bex_process::command(&self.program).ok()?;
        command
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
            .await
            .ok()?
            .ok()?;
        agent_providers::cli_version(&format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
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
