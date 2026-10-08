use agent_core::{state::Snapshot, store::Store};
use agent_transport::transport::{Endpoint, Relays, Ticket};
use host_daemon::local_host::{LocalHost, LocalHostRegistry, LocalHostState};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) mod task_menu;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(target_os = "windows")]
use windows as os;

mod microphone;
#[cfg(target_os = "macos")]
pub(crate) use microphone::prepare_microphone;
pub(crate) use microphone::{Recording, RecordingEvent, start_recording};

pub(crate) fn state_dir() -> Result<PathBuf, String> {
    std::env::var_os("BEX_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            directories::ProjectDirs::from("app", "bex", "BEX")
                .map(|project| project.data_local_dir().to_path_buf())
        })
        .ok_or_else(|| "application data directory unavailable".into())
}

pub(crate) async fn ssh_invitation(
    destination: &str,
) -> Result<agent_protocol::models::Invitation, String> {
    use tokio::io::AsyncReadExt;
    let destination = agent_core::presentation::connections::validate_ssh_destination(destination)?;
    let mut child = tokio::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=10",
            "--",
            destination,
            "sh -lc 'exec host-daemon invite'",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "SSHを起動できません。SSHがインストールされているか確認してください。")?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut contents = Vec::new();
        child.stdout.take().expect("SSH stdout is piped").take(64 * 1024 + 1)
            .read_to_end(&mut contents).await
            .map_err(|_| "SSHの応答を読み取れません。")?;
        if contents.len() > 64 * 1024 {
            return Err("SSHの応答が長すぎます。".into());
        }
        let status = child.wait().await.map_err(|_| "SSHの終了を確認できません。")?;
        if !status.success() {
            return Err("接続できません。SSHの鍵と接続先を確認し、サーバーでBex Hostを起動してください。".into());
        }
        serde_json::from_slice(&contents).map_err(|_| "Bexの接続情報を取得できません。接続先で host-daemon invite を実行できるか確認してください。".into())
    }).await;
    result.map_err(|_| "SSH接続がタイムアウトしました。".to_owned())?
}

/// One identity with separate local and remote endpoints. Views own sessions.
#[derive(Default)]
pub(crate) struct Connections {
    endpoint: tokio::sync::OnceCell<(PathBuf, Endpoint)>,
    local_endpoint: tokio::sync::OnceCell<(PathBuf, Endpoint)>,
    startup: tokio::sync::Mutex<()>,
}
impl Connections {
    pub(crate) async fn connect(
        self: &Arc<Self>,
        remote: Option<&str>,
        snapshot: Snapshot,
    ) -> anyhow::Result<Arc<Store>> {
        let startup = self.startup.lock().await;
        if let Some(remote) = remote {
            let ticket = remote.parse::<Ticket>()?;
            let endpoint = match self.endpoint.get() {
                Some((_, endpoint)) => endpoint,
                None => {
                    self.endpoint_for(&discover_local_host().await?.directory, false)
                        .await?
                }
            };
            drop(startup);
            return Ok(Arc::new(
                Store::connect(endpoint, &ticket, snapshot, None).await?,
            ));
        }
        let store = Arc::new(self.connect_local(snapshot).await?);
        self.recover_local(store.clone());
        Ok(store)
    }

    async fn endpoint_for(
        &self,
        directory: &std::path::Path,
        local: bool,
    ) -> anyhow::Result<&Endpoint> {
        let directory = tokio::fs::canonicalize(directory).await?;
        let cell = if local {
            &self.local_endpoint
        } else {
            &self.endpoint
        };
        let (identity_directory, endpoint) = cell
            .get_or_try_init(|| async {
                let identity_directory = directory.clone();
                let identity = tokio::task::spawn_blocking(move || {
                    host_daemon::load_local_identity(&identity_directory)
                })
                .await??;
                let endpoint = if local {
                    Endpoint::bind(identity, Relays::Loopback).await?
                } else {
                    Endpoint::bind(identity, Relays::Default).await?
                };
                Ok::<_, anyhow::Error>((directory.clone(), endpoint))
            })
            .await?;
        if identity_directory != &directory {
            return Err(anyhow::anyhow!(
                "Local Host changed; restart Bex to use its identity"
            ));
        }
        Ok(endpoint)
    }

    async fn connect_local(&self, snapshot: Snapshot) -> anyhow::Result<Store> {
        let isolated = isolated_host()?;
        let mut child = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut last_error = anyhow::anyhow!("Host startup timed out");
        let ready = async {
            loop {
                let location = discover_local_host().await?;
                match &location.state {
                    LocalHostState::Ready(ticket) => {
                        let endpoint = self.endpoint_for(&location.directory, true).await?;
                        let attempt = tokio::time::timeout(
                            Duration::from_secs(1),
                            Store::connect(endpoint, ticket, snapshot.clone(), None),
                        )
                        .await;
                        match attempt {
                            Ok(Ok(store)) => break Ok(store),
                            Ok(Err(error)) => last_error = error.into(),
                            Err(_) => {
                                last_error = anyhow::anyhow!("Host did not answer during startup")
                            }
                        }
                    }
                    LocalHostState::Stopped if child.is_none() => {
                        child = Some(start_host(&location.directory, isolated)?);
                    }
                    _ => {}
                }
                if let Some(child) = child.as_mut()
                    && let Some(status) = child.try_wait()?
                    && matches!(location.state, LocalHostState::Stopped)
                {
                    break Err(anyhow::anyhow!("Host exited during startup ({status})"));
                }
                if tokio::time::Instant::now() >= deadline {
                    break Err(last_error);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        .await;
        if let Some(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        ready
    }

    fn recover_local(self: &Arc<Self>, store: Arc<Store>) {
        let connections = self.clone();
        tokio::spawn(async move {
            let mut snapshots = store.subscribe();
            let mut delay = Duration::from_millis(250);
            while snapshots
                .wait_for(|snapshot| !snapshot.connected)
                .await
                .is_ok()
            {
                tokio::time::sleep(delay).await;
                if snapshots.has_changed().is_err() {
                    break;
                }
                if snapshots.borrow().connected {
                    delay = Duration::from_millis(250);
                    continue;
                }
                let result = tokio::time::timeout(Duration::from_secs(10), async {
                    let location = discover_local_host().await?;
                    let LocalHostState::Ready(ticket) = &location.state else {
                        return Err(anyhow::anyhow!("Local Host is not ready"));
                    };
                    let endpoint = connections.endpoint_for(&location.directory, true).await?;
                    store.resume(endpoint, ticket).await?;
                    Ok::<_, anyhow::Error>(())
                })
                .await;
                delay = if matches!(result, Ok(Ok(()))) {
                    Duration::from_millis(250)
                } else {
                    (delay * 2).min(Duration::from_secs(5))
                };
            }
        });
    }

    pub(crate) async fn close(&self) {
        for cell in [&self.local_endpoint, &self.endpoint] {
            if let Some((_, endpoint)) = cell.get() {
                endpoint.close().await;
            }
        }
    }
}

fn isolated_host() -> anyhow::Result<bool> {
    match std::env::var("BEX_ISOLATED_HOST").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("0") => Ok(false),
        Ok("1") if std::env::var_os("BEX_STATE_DIR").is_some() => Ok(true),
        Ok("1") => Err(anyhow::anyhow!(
            "BEX_ISOLATED_HOST=1 requires BEX_STATE_DIR"
        )),
        _ => Err(anyhow::anyhow!("BEX_ISOLATED_HOST must be 0 or 1")),
    }
}

async fn discover_local_host() -> anyhow::Result<LocalHost> {
    let preferred = state_dir().map_err(anyhow::Error::msg)?;
    let isolated = isolated_host()?;
    tokio::task::spawn_blocking(move || {
        let registry = if isolated {
            LocalHostRegistry::new(preferred.clone())
        } else {
            LocalHostRegistry::for_user()?
        };
        registry.resolve(&preferred)
    })
    .await?
}

fn start_host(directory: &std::path::Path, isolated: bool) -> anyhow::Result<std::process::Child> {
    let executable = std::env::var_os("BEX_HOST_DAEMON")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            std::env::current_exe().map(|path| {
                path.with_file_name(format!("host-daemon{}", std::env::consts::EXE_SUFFIX))
            })
        })?;
    let mut command = Command::new(executable);
    command
        .arg("--state-dir")
        .arg(directory)
        .arg("--codex")
        .arg(std::env::var_os("BEX_CODEX").unwrap_or_else(|| "codex".into()))
        .arg("--claude")
        .arg(std::env::var_os("BEX_CLAUDE").unwrap_or_else(|| "claude".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if isolated {
        command.arg("--isolated");
    }
    if let Some(directory) = std::env::var_os("BEX_ACCOUNT_STATE_DIR") {
        command.arg("--account-state-dir").arg(directory);
    }
    os::prepare_host(&mut command);
    command.spawn().map_err(Into::into)
}

pub(crate) fn choose_files() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new().pick_files()
}
pub(crate) fn choose_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}
pub(crate) fn choose_destination(name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_file_name(name).save_file()
}
