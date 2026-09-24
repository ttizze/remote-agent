use agent_core::{state::Snapshot, store::Store};
use agent_transport::transport::{Endpoint, Relays, Ticket};
use host_daemon::local_host::{LocalHost, LocalHostRegistry, LocalHostState};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(target_os = "windows")]
use windows as os;

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod microphone;
pub(crate) use os::{Recording, start_recording};
pub(crate) enum RecordingEvent {
    Started,
    Level(f32),
    Finished(Result<Vec<u8>, String>),
}

pub(crate) fn state_dir() -> Result<PathBuf, String> {
    std::env::var_os("BEX_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            directories::ProjectDirs::from("app", "bex", "BEX")
                .map(|project| project.data_local_dir().to_path_buf())
        })
        .ok_or_else(|| "application data directory unavailable".into())
}

/// One identity and endpoint per app. Views own independent sessions.
#[derive(Default)]
pub(crate) struct Connections {
    endpoint: tokio::sync::OnceCell<(PathBuf, host_daemon::KeyStorage, Endpoint)>,
    startup: tokio::sync::Mutex<()>,
}
impl Connections {
    pub(crate) async fn connect(
        &self,
        remote: Option<&str>,
        snapshot: Snapshot,
    ) -> anyhow::Result<Store> {
        let startup = self.startup.lock().await;
        if let Some(remote) = remote {
            let ticket = remote.parse::<Ticket>()?;
            let endpoint = match self.endpoint.get() {
                Some((_, _, endpoint)) => endpoint,
                None => self.endpoint_for(&discover_local_host().await?).await?,
            };
            drop(startup);
            return Store::connect(endpoint, &ticket, snapshot, None)
                .await
                .map_err(Into::into);
        }
        self.connect_local(snapshot).await
    }

    async fn endpoint_for(&self, host: &LocalHost) -> anyhow::Result<&Endpoint> {
        let directory = tokio::fs::canonicalize(&host.directory).await?;
        let storage = host.key_storage.unwrap_or_default();
        let (identity_directory, identity_storage, endpoint) = self
            .endpoint
            .get_or_try_init(|| async {
                let mut host = host.clone();
                host.directory = directory.clone();
                let identity = tokio::task::spawn_blocking(move || host.load_identity()).await??;
                let endpoint = Endpoint::bind(identity, Relays::Default).await?;
                Ok::<_, anyhow::Error>((directory.clone(), storage, endpoint))
            })
            .await?;
        if identity_directory != &directory || identity_storage != &storage {
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
                        let endpoint = self.endpoint_for(&location).await?;
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
                        child = Some(start_host(&location, isolated)?);
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

    pub(crate) async fn close(&self) {
        if let Some((_, _, endpoint)) = self.endpoint.get() {
            endpoint.close().await;
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

fn start_host(host: &LocalHost, isolated: bool) -> anyhow::Result<std::process::Child> {
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
        .arg(&host.directory)
        .arg("--codex")
        .arg(std::env::var_os("BEX_CODEX").unwrap_or_else(|| "codex".into()))
        .arg("--claude")
        .arg(std::env::var_os("BEX_CLAUDE").unwrap_or_else(|| "claude".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let key_storage = match host.key_storage {
        Some(host_daemon::KeyStorage::File) => Some("file".into()),
        Some(host_daemon::KeyStorage::Keyring) => Some("keyring".into()),
        None => std::env::var_os("BEX_KEY_STORAGE"),
    };
    if let Some(storage) = key_storage {
        command.arg("--key-storage").arg(storage);
    }
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
