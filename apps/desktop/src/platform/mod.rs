use agent_core::{
    connection::{Store, StoreOptions},
    state::Snapshot,
};
use agent_protocol::models::UpdateTarget;
use agent_transport::transport::{Endpoint, Relays, Ticket};
use anyhow::Context;
use host_daemon::local_host::{LocalHost, LocalHostRegistry, LocalHostState};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const DESKTOP_HANDOFF_TTL_SECS: u64 = 300;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DesktopHandoffAttempt {
    executable: PathBuf,
    version: String,
    owner_pid: u32,
    created_at: u64,
}

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
        .map_err(|_| "SSH could not start. Check that SSH is installed.")?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut contents = Vec::new();
        child.stdout.take().expect("SSH stdout is piped").take(64 * 1024 + 1)
            .read_to_end(&mut contents).await
            .map_err(|_| "The SSH response could not be read.")?;
        if contents.len() > 64 * 1024 {
            return Err("The SSH response is too long.".into());
        }
        let status = child.wait().await.map_err(|_| "SSH did not report how it exited.")?;
        if !status.success() {
            return Err("Cannot connect. Check the SSH key and destination, and start the Bex Host on the server.".into());
        }
        serde_json::from_slice(&contents).map_err(|_| "The pairing details could not be read. Check that host-daemon invite runs on the server.".into())
    }).await;
    result.map_err(|_| "The SSH connection timed out.".to_owned())?
}

/// One identity with separate local and remote endpoints. Views own sessions.
#[derive(Default)]
pub(crate) struct Connections {
    endpoint: tokio::sync::OnceCell<(PathBuf, Endpoint)>,
    local_endpoint: tokio::sync::OnceCell<(PathBuf, Endpoint)>,
    startup: tokio::sync::Mutex<()>,
}

/// The connection path is part of the power-publisher safety boundary.  A
/// local session is marked only after the registry-backed local Host answered
/// on its loopback endpoint; a remote ticket can never acquire this marker.
pub(crate) struct ConnectedStore {
    pub(crate) store: Arc<Store>,
    pub(crate) local_host_supervised: bool,
}

impl Connections {
    pub(crate) async fn connect(
        self: &Arc<Self>,
        remote: Option<&str>,
        snapshot: Snapshot,
        options: StoreOptions,
    ) -> anyhow::Result<ConnectedStore> {
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
            return Ok(ConnectedStore {
                store: Arc::new(Store::connect(endpoint, &ticket, snapshot, options, None).await?),
                local_host_supervised: false,
            });
        }
        let store = Arc::new(self.connect_local(snapshot, options).await?);
        self.recover_local(store.clone());
        Ok(ConnectedStore {
            store,
            local_host_supervised: true,
        })
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

    async fn connect_local(
        &self,
        snapshot: Snapshot,
        options: StoreOptions,
    ) -> anyhow::Result<Store> {
        let isolated = isolated_host()?;
        let mut child = None;
        let mut host_handoff_attempted = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut last_error = anyhow::anyhow!("Host startup timed out");
        let ready = async {
            loop {
                let location = discover_local_host().await?;
                if !host_handoff_attempted
                    && !matches!(&location.state, LocalHostState::Stopped)
                    && resolve_installed_host_executable(&location.directory)?.is_some()
                {
                    host_handoff_attempted = true;
                    let directory = location.directory.clone();
                    let stopped = tokio::task::spawn_blocking(move || {
                        handoff_installed_host(&directory, false, isolated)
                    })
                    .await??;
                    host_handoff_attempted = stopped;
                    if !stopped {
                        last_error =
                            anyhow::anyhow!("Installed Host is waiting for active work to settle");
                    }
                    continue;
                }
                if resolve_installed_host_executable(&location.directory)?.is_some()
                    && !matches!(&location.state, LocalHostState::Stopped)
                {
                    // Do not reconnect the old Host after a handoff timeout;
                    // retry while the target-owned transaction remains pending.
                    host_handoff_attempted = false;
                    if tokio::time::Instant::now() >= deadline {
                        break Err(last_error);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                match &location.state {
                    LocalHostState::Ready(ticket) => {
                        let endpoint = self.endpoint_for(&location.directory, true).await?;
                        let attempt = tokio::time::timeout(
                            Duration::from_secs(1),
                            Store::connect(
                                endpoint,
                                ticket,
                                snapshot.clone(),
                                options.clone(),
                                None,
                            ),
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
                    && matches!(&location.state, LocalHostState::Stopped)
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

const UPDATE_HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait for the existing shared Host to release its lease. Only the Host can
/// decide that provider work is settled; Desktop requests a drain and never
/// kills or replaces the running process itself.
fn wait_for_host_stop(
    directory: &std::path::Path,
    initially_stopped: bool,
    registry: &LocalHostRegistry,
    preferred: &std::path::Path,
    target: UpdateTarget,
    version: &str,
) -> anyhow::Result<bool> {
    if initially_stopped {
        return Ok(true);
    }
    let created = host_daemon::request_update_handoff(directory, target, version)?;
    let deadline = Instant::now() + UPDATE_HANDOFF_TIMEOUT;
    loop {
        let current = registry.resolve(preferred)?;
        if current.directory.as_path() != directory {
            if created {
                let _ = host_daemon::clear_update_handoff(directory);
            }
            return Ok(false);
        }
        if matches!(current.state, LocalHostState::Stopped) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            if created {
                host_daemon::clear_update_handoff(directory)?;
            }
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn handoff_installed_host(
    directory: &std::path::Path,
    initially_stopped: bool,
    isolated: bool,
) -> anyhow::Result<bool> {
    let preferred = state_dir().map_err(anyhow::Error::msg)?;
    let registry = if isolated {
        LocalHostRegistry::new(preferred.clone())
    } else {
        LocalHostRegistry::for_user()?
    };
    let version = pending_update_version(directory, UpdateTarget::Host)?
        .context("installed Host has no matching update transaction")?;
    wait_for_host_stop(
        directory,
        initially_stopped,
        &registry,
        &preferred,
        UpdateTarget::Host,
        &version,
    )
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

fn start_host(location: &LocalHost, isolated: bool) -> anyhow::Result<std::process::Child> {
    anyhow::ensure!(
        matches!(&location.state, LocalHostState::Stopped),
        "cannot start a Host while it is active"
    );
    let directory = &location.directory;
    let executable = std::env::var_os("BEX_HOST_DAEMON")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            std::env::current_exe().map(|path| {
                let bundled =
                    path.with_file_name(format!("host-daemon{}", std::env::consts::EXE_SUFFIX));
                resolve_installed_host_executable(directory)
                    .unwrap_or(None)
                    .unwrap_or(bundled)
            })
        })?;
    let mut command = Command::new(&executable);
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

/// Resolve a verified executable installed by the Host update owner. A
/// malformed or unsafe marker is ignored and the caller keeps its bundled
/// executable.
fn resolve_installed_executable(
    directory: &std::path::Path,
    target: &str,
    relative_executable: &std::path::Path,
) -> anyhow::Result<Option<PathBuf>> {
    let transaction = directory
        .join("transactions")
        .join(format!("{target}.json"));
    let bytes = match std::fs::read(transaction) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let document: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(document) => document,
        Err(_) => return Ok(None),
    };
    let state = document.get("state");
    let restart_required = state
        .and_then(|state| state.get("restartRequired"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !restart_required {
        return Ok(None);
    }
    let Some(version) = state
        .and_then(|state| state.get("downloadedVersion"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    if version.is_empty()
        || version == "."
        || version == ".."
        || version
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0'))
    {
        return Ok(None);
    }
    let installed_root = directory.join("installed");
    let target_root = installed_root.join(target);
    let version_root = target_root.join(version);
    let executable = version_root.join(relative_executable);
    let installed_metadata = match std::fs::symlink_metadata(&installed_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let root_metadata = match std::fs::symlink_metadata(&target_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let version_metadata = match std::fs::symlink_metadata(&version_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !installed_metadata.is_dir()
        || installed_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || root_metadata.file_type().is_symlink()
        || !version_metadata.is_dir()
        || version_metadata.file_type().is_symlink()
    {
        return Ok(None);
    }
    let mut current = version_root;
    let mut components = relative_executable.components().peekable();
    #[cfg(unix)]
    let mut executable_metadata = None;
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            return Ok(None);
        };
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let is_executable = components.peek().is_none();
        if metadata.file_type().is_symlink()
            || (!is_executable && !metadata.is_dir())
            || (is_executable && !metadata.is_file())
        {
            return Ok(None);
        }
        #[cfg(unix)]
        if is_executable {
            executable_metadata = Some(metadata);
        }
    }
    let canonical_root = std::fs::canonicalize(&target_root)?;
    let canonical_executable = std::fs::canonicalize(&executable)?;
    if !canonical_executable.starts_with(&canonical_root) {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if executable_metadata
            .expect("the executable path was checked above")
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Ok(None);
        }
    }
    Ok(Some(executable))
}

fn resolve_installed_host_executable(
    directory: &std::path::Path,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(executable) = resolve_installed_executable(
        directory,
        "host",
        &PathBuf::from(format!("host-daemon{}", std::env::consts::EXE_SUFFIX)),
    )?
    else {
        return Ok(None);
    };
    let supervisor = executable
        .parent()
        .expect("installed Host executable has a version directory")
        .join(format!(
            "bex-provider-supervisor{}",
            std::env::consts::EXE_SUFFIX
        ));
    let metadata = std::fs::symlink_metadata(&supervisor).ok();
    if !metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata
            .expect("the supervisor path was checked above")
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Ok(None);
        }
    }
    Ok(Some(executable))
}

fn resolve_installed_desktop_executable(
    directory: &std::path::Path,
) -> anyhow::Result<Option<PathBuf>> {
    #[cfg(target_os = "macos")]
    let relative = PathBuf::from("Bex.app/Contents/MacOS/Bex");
    #[cfg(not(target_os = "macos"))]
    let relative = PathBuf::from(format!("desktop{}", std::env::consts::EXE_SUFFIX));
    resolve_installed_executable(directory, "desktop", &relative)
}

fn desktop_handoff_attempt_path(directory: &std::path::Path) -> PathBuf {
    directory.join("transactions/desktop-handoff.json")
}

fn write_desktop_handoff_attempt(
    directory: &std::path::Path,
    executable: &std::path::Path,
    version: &str,
) -> anyhow::Result<bool> {
    let path = desktop_handoff_attempt_path(directory);
    let parent = path
        .parent()
        .expect("desktop handoff marker has a transaction directory");
    host_daemon::platform::create_state_directory(parent)?;
    let bytes = serde_json::to_vec(&DesktopHandoffAttempt {
        executable: executable.to_owned(),
        version: version.to_owned(),
        owner_pid: std::process::id(),
        created_at: unix_now(),
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut marker = match options.open(&path) {
        Ok(marker) => marker,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = marker.write_all(&bytes).and_then(|_| marker.sync_all()) {
        let _ = std::fs::remove_file(&path);
        return Err(error.into());
    }
    Ok(true)
}

fn acknowledge_desktop_handoff_at(
    directory: &std::path::Path,
    current_executable: &std::path::Path,
) -> anyhow::Result<bool> {
    if !desktop_handoff_attempt_matches(directory, current_executable)? {
        return Ok(false);
    }
    std::fs::remove_file(desktop_handoff_attempt_path(directory))?;
    Ok(true)
}

fn desktop_handoff_attempt_matches(
    directory: &std::path::Path,
    current_executable: &std::path::Path,
) -> anyhow::Result<bool> {
    let path = desktop_handoff_attempt_path(directory);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let attempt: DesktopHandoffAttempt = match serde_json::from_slice(&bytes) {
        Ok(attempt) => attempt,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            return Ok(false);
        }
    };
    let current = std::fs::canonicalize(current_executable)?;
    let expected = match std::fs::canonicalize(&attempt.executable) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let _ = std::fs::remove_file(&path);
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    if current != expected {
        return Ok(false);
    }
    Ok(true)
}

fn pending_update_version(
    directory: &std::path::Path,
    target: UpdateTarget,
) -> anyhow::Result<Option<String>> {
    let target_name = match target {
        UpdateTarget::Host => "host",
        UpdateTarget::Desktop => "desktop",
    };
    let path = directory
        .join("transactions")
        .join(format!("{}.json", target_name));
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let document = serde_json::from_slice::<serde_json::Value>(&bytes)?;
    let Some(version) = document
        .get("state")
        .and_then(|state| state.get("downloadedVersion"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    Ok((!version.is_empty()).then(|| version.to_owned()))
}

fn desktop_handoff_attempt_is_live(attempt: &DesktopHandoffAttempt) -> bool {
    if attempt.owner_pid == 0
        || unix_now().saturating_sub(attempt.created_at) > DESKTOP_HANDOFF_TTL_SECS
    {
        return false;
    }
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(attempt.owner_pid as libc::pid_t, 0) };
        return result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn acknowledge_installed_desktop_handoff() -> anyhow::Result<bool> {
    let preferred = state_dir().map_err(anyhow::Error::msg)?;
    let isolated = isolated_host()?;
    let registry = if isolated {
        LocalHostRegistry::new(preferred.clone())
    } else {
        LocalHostRegistry::for_user()?
    };
    let location = registry.resolve(&preferred)?;
    let current = std::env::current_exe()?;
    if !desktop_handoff_attempt_matches(&location.directory, &current)? {
        return Ok(false);
    }
    let cleared = host_daemon::acknowledge_update_target(
        &location.directory,
        UpdateTarget::Desktop,
        &host_daemon::current_update_version(),
    )?;
    if !cleared && desktop_update_still_requires_ack(&location.directory) {
        // Keep the target-owned marker until the paired build can prove its
        // version. This prevents a mismatched binary from consuming the
        // restart requirement merely by sharing the executable path.
        return Ok(false);
    }
    std::fs::remove_file(desktop_handoff_attempt_path(&location.directory))?;
    Ok(true)
}

fn desktop_update_still_requires_ack(directory: &std::path::Path) -> bool {
    std::fs::read(directory.join("transactions/desktop.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|document| document.get("state").cloned())
        .and_then(|state| state.get("restartRequired").cloned())
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Hand off to an installed Desktop update before the old application starts.
/// The registry lock makes a running Host a safe stop condition: the bundled
/// application remains in place until the Host and its active tasks have
/// stopped. The installed binary receives a marker so it does not hand off to
/// itself again.
pub(crate) fn handoff_installed_desktop() -> anyhow::Result<bool> {
    if std::env::var_os("APP_DESKTOP_UPDATE_HANDOFF").is_some() {
        return Ok(false);
    }
    let preferred = state_dir().map_err(anyhow::Error::msg)?;
    let isolated = isolated_host()?;
    let registry = if isolated {
        LocalHostRegistry::new(preferred.clone())
    } else {
        LocalHostRegistry::for_user()?
    };
    let location = registry.resolve(&preferred)?;
    let handoff_attempt = desktop_handoff_attempt_path(&location.directory);
    let Some(version) = pending_update_version(&location.directory, UpdateTarget::Desktop)? else {
        let _ = std::fs::remove_file(&handoff_attempt);
        return Ok(false);
    };
    let Some(installed) = resolve_installed_desktop_executable(&location.directory)? else {
        let _ = std::fs::remove_file(&handoff_attempt);
        return Ok(false);
    };
    if handoff_attempt.exists() {
        let pending = std::fs::read(&handoff_attempt)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DesktopHandoffAttempt>(&bytes).ok())
            .is_some_and(|attempt| {
                attempt.version == version
                    && std::fs::canonicalize(&attempt.executable).ok()
                        == std::fs::canonicalize(&installed).ok()
                    && desktop_handoff_attempt_is_live(&attempt)
            });
        if pending {
            return Ok(false);
        }
        let _ = std::fs::remove_file(&handoff_attempt);
    }
    let current = std::env::current_exe()?;
    if std::fs::canonicalize(&current).ok() == std::fs::canonicalize(&installed).ok() {
        return Ok(false);
    }
    if !write_desktop_handoff_attempt(&location.directory, &installed, &version)? {
        return Ok(false);
    }
    if !wait_for_host_stop(
        &location.directory,
        matches!(&location.state, LocalHostState::Stopped),
        &registry,
        &preferred,
        UpdateTarget::Desktop,
        &version,
    )? {
        let _ = std::fs::remove_file(desktop_handoff_attempt_path(&location.directory));
        return Ok(false);
    }
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let mut command = Command::new(&installed);
    command
        .args(arguments)
        .env("APP_DESKTOP_UPDATE_HANDOFF", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(error) = command.spawn() {
        let _ = std::fs::remove_file(desktop_handoff_attempt_path(&location.directory));
        return Err(error.into());
    }
    Ok(true)
}

#[cfg(test)]
mod update_handoff_tests {
    use super::{
        DesktopHandoffAttempt, acknowledge_desktop_handoff_at,
        resolve_installed_desktop_executable, resolve_installed_host_executable,
        write_desktop_handoff_attempt,
    };
    use std::{fs, path::PathBuf};

    #[test]
    fn selects_verified_installed_host_after_a_completed_install() {
        let directory = tempfile::tempdir().expect("temporary Host state directory");
        let version = "0.1.0-nightly.20261008.42";
        let version_root = directory.path().join("installed/host").join(version);
        fs::create_dir_all(&version_root).expect("installed version directory");
        let executable = version_root.join(format!("host-daemon{}", std::env::consts::EXE_SUFFIX));
        fs::write(&executable, b"verified executable").expect("installed Host executable");
        let supervisor = version_root.join(format!(
            "bex-provider-supervisor{}",
            std::env::consts::EXE_SUFFIX
        ));
        fs::write(&supervisor, b"verified supervisor").expect("installed supervisor");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("make test executable runnable");
            fs::set_permissions(&supervisor, fs::Permissions::from_mode(0o755))
                .expect("make test supervisor runnable");
        }
        let transaction_root = directory.path().join("transactions");
        fs::create_dir_all(&transaction_root).expect("transaction directory");
        fs::write(
            transaction_root.join("host.json"),
            format!(
                "{{\"state\":{{\"restartRequired\":true,\"downloadedVersion\":\"{version}\"}}}}"
            ),
        )
        .expect("handoff marker");
        assert_eq!(
            resolve_installed_host_executable(directory.path()).expect("handoff read"),
            Some(executable),
        );
    }

    #[test]
    fn ignores_a_symlinked_installed_host() {
        let directory = tempfile::tempdir().expect("temporary Host state directory");
        let version = "0.1.0";
        let version_root = directory.path().join("installed/host").join(version);
        fs::create_dir_all(&version_root).expect("installed version directory");
        let outside = directory.path().join("outside-host");
        fs::write(&outside, b"outside executable").expect("outside executable");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            &outside,
            version_root.join(format!("host-daemon{}", std::env::consts::EXE_SUFFIX)),
        )
        .expect("symlink fixture");
        #[cfg(windows)]
        return;
        fs::create_dir_all(directory.path().join("transactions")).expect("transaction directory");
        fs::write(
            directory.path().join("transactions/host.json"),
            format!(
                "{{\"state\":{{\"restartRequired\":true,\"downloadedVersion\":\"{version}\"}}}}"
            ),
        )
        .expect("handoff marker");
        assert_eq!(
            resolve_installed_host_executable(directory.path()).expect("handoff read"),
            None,
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignores_a_non_executable_installed_host() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary Host state directory");
        let version = "0.1.0";
        let version_root = directory.path().join("installed/host").join(version);
        fs::create_dir_all(&version_root).expect("installed version directory");
        let executable = version_root.join("host-daemon");
        fs::write(&executable, b"not executable").expect("installed Host executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o600))
            .expect("make test executable non-runnable");
        fs::create_dir_all(directory.path().join("transactions")).expect("transaction directory");
        fs::write(
            directory.path().join("transactions/host.json"),
            format!(
                "{{\"state\":{{\"restartRequired\":true,\"downloadedVersion\":\"{version}\"}}}}"
            ),
        )
        .expect("handoff marker");
        assert_eq!(
            resolve_installed_host_executable(directory.path()).expect("handoff read"),
            None,
        );
    }

    #[test]
    fn selects_the_installed_desktop_executable_after_a_completed_install() {
        let directory = tempfile::tempdir().expect("temporary Desktop state directory");
        let version = "0.1.0";
        #[cfg(target_os = "macos")]
        let relative = std::path::PathBuf::from("Bex.app/Contents/MacOS/Bex");
        #[cfg(not(target_os = "macos"))]
        let relative = std::path::PathBuf::from(format!("desktop{}", std::env::consts::EXE_SUFFIX));
        let executable = directory
            .path()
            .join("installed/desktop")
            .join(version)
            .join(&relative);
        fs::create_dir_all(executable.parent().expect("desktop executable parent"))
            .expect("installed Desktop directory");
        fs::write(&executable, b"verified desktop executable")
            .expect("installed Desktop executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("make test executable runnable");
        }
        let transaction_root = directory.path().join("transactions");
        fs::create_dir_all(&transaction_root).expect("transaction directory");
        fs::write(
            transaction_root.join("desktop.json"),
            format!(
                "{{\"state\":{{\"restartRequired\":true,\"downloadedVersion\":\"{version}\"}}}}"
            ),
        )
        .expect("handoff marker");
        assert_eq!(
            resolve_installed_desktop_executable(directory.path()).expect("handoff read"),
            Some(executable),
        );
    }

    #[test]
    fn desktop_handoff_waits_for_the_installed_process_to_acknowledge() {
        let directory = tempfile::tempdir().expect("temporary Desktop state directory");
        let expected = directory.path().join("installed-desktop");
        let current = directory.path().join("current-desktop");
        fs::write(&expected, b"installed").expect("installed executable");
        fs::write(&current, b"current").expect("current executable");
        assert!(
            write_desktop_handoff_attempt(directory.path(), &expected, "1.0.0")
                .expect("write marker")
        );
        assert!(
            !write_desktop_handoff_attempt(directory.path(), &current, "1.0.0")
                .expect("claim marker")
        );
        assert!(!acknowledge_desktop_handoff_at(directory.path(), &current).expect("old app"));
        assert!(acknowledge_desktop_handoff_at(directory.path(), &expected).expect("new app"));
        assert!(!super::desktop_handoff_attempt_path(directory.path()).exists());
    }

    #[test]
    fn expired_desktop_handoff_owner_does_not_block_a_new_launch() {
        let attempt = DesktopHandoffAttempt {
            executable: PathBuf::from("installed-desktop"),
            version: "1.0.0".into(),
            owner_pid: std::process::id(),
            created_at: 0,
        };
        assert!(!super::desktop_handoff_attempt_is_live(&attempt));
    }
}

pub(crate) fn choose_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}

pub(crate) fn snapshot_metadata_path(path: &std::path::Path) -> PathBuf {
    PathBuf::from(format!("{}.meta.json", path.to_string_lossy()))
}

const SNAPSHOT_QUEUE_MAX_METADATA_BYTES: u64 = 128 * 1024;
const SNAPSHOT_QUEUE_MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct PendingSnapshot {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) mime_type: String,
    pub(crate) size_bytes: u64,
    pub(crate) source: agent_domain::CapturedWindow,
    pub(crate) path: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotMetadata {
    id: String,
    name: String,
    mime_type: String,
    size_bytes: u64,
    source: SnapshotSource,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotSource {
    app_name: String,
    window_title: String,
    #[serde(default)]
    accessible_text: Option<String>,
    #[serde(default)]
    accessibility: Option<serde_json::Value>,
}

fn decode_snapshot_accessibility(
    value: Option<serde_json::Value>,
) -> Option<agent_domain::Accessibility> {
    let value = value?;
    if let Ok(accessibility) = serde_json::from_value(value.clone()) {
        return Some(accessibility);
    }
    let mut object = value.as_object()?.clone();
    let format = object.remove("format")?.as_str()?.to_owned();
    let tag = match format.as_str() {
        "flat-text" => "flat-text",
        "element-tree" => "element-tree",
        _ => return None,
    };
    let mut tagged = serde_json::Map::new();
    tagged.insert(tag.to_owned(), serde_json::Value::Object(object));
    serde_json::from_value(serde_json::Value::Object(tagged)).ok()
}

impl SnapshotMetadata {
    fn into_pending(self, path: PathBuf) -> Option<PendingSnapshot> {
        let valid = valid_snapshot_id(&self.id)
            && self.mime_type.eq_ignore_ascii_case("image/png")
            && !self.name.trim().is_empty()
            && self.name.len() <= 255
            && Path::new(&self.name)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(self.name.as_str())
            && self.name.to_ascii_lowercase().ends_with(".png")
            && self.size_bytes > 0
            && self.size_bytes <= SNAPSHOT_QUEUE_MAX_IMAGE_BYTES
            && self.source.app_name.len() <= 512
            && self.source.window_title.len() <= 2_048
            && self
                .source
                .accessible_text
                .as_ref()
                .is_none_or(|text| text.len() <= 64 * 1024);
        valid.then_some(PendingSnapshot {
            id: self.id,
            name: self.name,
            mime_type: "image/png".into(),
            size_bytes: self.size_bytes,
            source: agent_domain::CapturedWindow {
                app_name: self.source.app_name,
                window_title: self.source.window_title,
                accessible_text: self.source.accessible_text,
                accessibility: decode_snapshot_accessibility(self.source.accessibility),
            },
            path,
        })
    }
}

fn valid_snapshot_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-')
}

fn snapshot_queue_directory() -> Result<PathBuf, String> {
    Ok(state_dir()?.join("snap-shots"))
}

fn read_snapshot_metadata(path: &Path) -> Result<SnapshotMetadata, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > SNAPSHOT_QUEUE_MAX_METADATA_BYTES {
        return Err("snapshot metadata is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("snapshot metadata is invalid: {error}"))
}

pub(crate) fn pending_snapshots() -> Result<Vec<PendingSnapshot>, String> {
    let directory = snapshot_queue_directory()?;
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Ok(vec![]);
    };
    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata_path = entry.path();
        if metadata_path.extension().and_then(|value| value.to_str()) != Some("json")
            || metadata_path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.ends_with(".json.tmp"))
        {
            continue;
        }
        let Ok(metadata) = read_snapshot_metadata(&metadata_path) else {
            continue;
        };
        if !valid_snapshot_id(&metadata.id) {
            continue;
        }
        let image_path = directory.join(format!("{}.png", metadata.id));
        let Ok(image) = std::fs::metadata(&image_path) else {
            continue;
        };
        if !image.is_file() || image.len() != metadata.size_bytes {
            continue;
        }
        let Some(snapshot) = metadata.into_pending(image_path) else {
            continue;
        };
        snapshots.push(snapshot);
    }
    snapshots.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(snapshots)
}

pub(crate) fn read_pending_snapshot(id: &str) -> Result<PendingSnapshot, String> {
    if !valid_snapshot_id(id) {
        return Err("snapshot id is invalid".into());
    }
    let directory = snapshot_queue_directory()?;
    let metadata_path = directory.join(format!("{id}.json"));
    let metadata = read_snapshot_metadata(&metadata_path)?;
    if metadata.id != id {
        return Err("snapshot metadata id does not match its path".into());
    }
    let image_path = directory.join(format!("{id}.png"));
    let image = std::fs::metadata(&image_path).map_err(|error| error.to_string())?;
    if !image.is_file() || image.len() != metadata.size_bytes {
        return Err("snapshot image is incomplete".into());
    }
    metadata
        .into_pending(image_path)
        .ok_or_else(|| "snapshot metadata is invalid".into())
}

pub(crate) fn acknowledge_snapshot(id: &str) -> Result<(), String> {
    if !valid_snapshot_id(id) {
        return Err("snapshot id is invalid".into());
    }
    let directory = snapshot_queue_directory()?;
    for path in [
        directory.join(format!("{id}.json")),
        directory.join(format!("{id}.png")),
    ] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Returns the active window identity used by screenshot attachments. The
/// bridge deliberately records only the foreground application and window;
/// accessibility text is collected only when the setting is enabled and the
/// platform grants access.
const SNAPSHOT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const SNAPSHOT_MAX_OUTPUT_BYTES: usize = 128 * 1024;

struct SnapshotCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn read_snapshot_output<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;

    let mut kept = Vec::with_capacity(SNAPSHOT_MAX_OUTPUT_BYTES);
    let mut buffer = [0u8; 4096];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("screen capture output could not be read: {error}"))?;
        if read == 0 {
            return Ok(kept);
        }
        let remaining = SNAPSHOT_MAX_OUTPUT_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

async fn run_snapshot_command(
    mut command: tokio::process::Command,
) -> Result<SnapshotCommandOutput, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("screen capture could not start: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "screen capture stdout is unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "screen capture stderr is unavailable".to_owned())?;
    let result = tokio::time::timeout(SNAPSHOT_COMMAND_TIMEOUT, async {
        let (stdout, stderr, status) = tokio::join!(
            read_snapshot_output(stdout),
            read_snapshot_output(stderr),
            child.wait(),
        );
        Ok::<_, String>((
            stdout?,
            stderr?,
            status.map_err(|error| format!("screen capture did not report its status: {error}"))?,
        ))
    })
    .await
    .map_err(|_| "screen capture timed out".to_owned())??;
    Ok(SnapshotCommandOutput {
        stdout: result.0,
        stderr: result.1,
        status: result.2,
    })
}

fn snapshot_command_failed(name: &str, output: &SnapshotCommandOutput) -> String {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.is_empty() {
        format!("{name} exited with {}", output.status)
    } else {
        format!("{name} exited with {}: {detail}", output.status)
    }
}

async fn active_window_metadata(
    include_accessibility: bool,
) -> Option<agent_domain::CapturedWindow> {
    #[cfg(target_os = "macos")]
    {
        let accessibility = if include_accessibility {
            "set capturedText to \"\"\n    try\n        set capturedText to (value of every static text of front window of frontProcess) as text\n    end try"
        } else {
            "set capturedText to \"\""
        };
        let script = format!(
            "tell application \"System Events\"\n    set frontProcess to first application process whose frontmost is true\n    set appName to name of frontProcess\n    set windowTitle to \"\"\n    try\n        set windowTitle to name of front window of frontProcess\n    end try\n    {accessibility}\n    return appName & linefeed & windowTitle & linefeed & capturedText\nend tell"
        );
        let output =
            run_snapshot_command(tokio::process::Command::new("osascript").args(["-e", &script]))
                .await
                .ok()?;
        if !output.status.success() {
            return None;
        }
        let mut lines = String::from_utf8_lossy(&output.stdout).lines();
        let app_name = lines.next()?.trim().to_owned();
        let window_title = lines.next().unwrap_or_default().trim().to_owned();
        let mut accessible_text = lines.collect::<Vec<_>>().join(" ").trim().to_owned();
        let mut characters = accessible_text.chars();
        let limited_text: String = characters.by_ref().take(64 * 1024).collect();
        let truncated = characters.next().is_some();
        accessible_text = limited_text;
        let accessibility_text = accessible_text.clone();
        (!app_name.is_empty() || !window_title.is_empty()).then_some(agent_domain::CapturedWindow {
            app_name,
            window_title,
            accessible_text: (!accessible_text.is_empty()).then_some(accessible_text),
            accessibility: (include_accessibility && !accessibility_text.is_empty()).then_some(
                agent_domain::Accessibility::FlatText {
                    text: accessibility_text,
                    truncated,
                },
            ),
        })
    }
    #[cfg(target_os = "linux")]
    {
        let id =
            run_snapshot_command(tokio::process::Command::new("xdotool").arg("getactivewindow"))
                .await
                .ok()
                .filter(|output| output.status.success())?;
        let id = String::from_utf8_lossy(&id.stdout).trim().to_owned();
        if id.is_empty() {
            return None;
        }
        let title = run_snapshot_command(
            tokio::process::Command::new("xdotool").args(["getwindowname", &id]),
        )
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
        let app_name = run_snapshot_command(
            tokio::process::Command::new("xprop").args(["-id", &id, "WM_CLASS"]),
        )
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split('"')
                .nth(3)
                .map(str::to_owned)
        })
        .unwrap_or_default();
        (!app_name.is_empty() || !title.is_empty()).then_some(agent_domain::CapturedWindow {
            app_name,
            window_title: title,
            accessible_text: None,
            accessibility: None,
        })
    }
    #[cfg(target_os = "windows")]
    {
        let _ = include_accessibility;
        let script = r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class RemoteAgentWindow {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
}
"@
$window = [RemoteAgentWindow]::GetForegroundWindow()
$titleBuffer = New-Object System.Text.StringBuilder 2048
[void][RemoteAgentWindow]::GetWindowText($window, $titleBuffer, $titleBuffer.Capacity)
$processId = 0
[void][RemoteAgentWindow]::GetWindowThreadProcessId($window, [ref]$processId)
$app = ""
try { $app = [Diagnostics.Process]::GetProcessById($processId).ProcessName } catch {}
Write-Output $app
Write-Output $titleBuffer.ToString()
"#;
        let output = run_snapshot_command(tokio::process::Command::new("powershell").args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]))
        .await
        .ok()
        .filter(|output| output.status.success())?;
        let mut lines = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim);
        let app_name = lines.next().unwrap_or_default().to_owned();
        let window_title = lines.next().unwrap_or_default().to_owned();
        (!app_name.is_empty() || !window_title.is_empty()).then_some(agent_domain::CapturedWindow {
            app_name,
            window_title,
            accessible_text: None,
            accessibility: None,
        })
    }
}

pub(crate) async fn snapshot_permission_granted(include_accessibility: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        !include_accessibility
            || run_snapshot_command(tokio::process::Command::new("osascript").args([
                "-e",
                "tell application \"System Events\" to get name of first application process whose frontmost is true",
            ]))
            .await
            .is_ok_and(|output| output.status.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = include_accessibility;
        true
    }
}

fn write_snapshot_metadata(
    path: &std::path::Path,
    window: Option<&agent_domain::CapturedWindow>,
) -> Result<(), String> {
    let Some(window) = window else {
        return Ok(());
    };
    serde_json::to_vec(&window)
        .map_err(|error| format!("snapshot metadata could not be encoded: {error}"))
        .and_then(|bytes| {
            std::fs::write(snapshot_metadata_path(path), bytes)
                .map_err(|error| format!("snapshot metadata could not be saved: {error}"))
        })
}

/// Captures the desktop into a caller-owned path using the platform's native
/// screen capture utility. The caller feeds the resulting file into the same
/// attachment admission path as a picked image.
pub(crate) async fn capture_snapshot(
    path: &std::path::Path,
    include_accessibility: bool,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let window = active_window_metadata(include_accessibility).await;
        let command = tokio::process::Command::new("screencapture")
            .args(["-x", "-w", "-t", "png"])
            .arg(path);
        let output = run_snapshot_command(command).await?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| snapshot_command_failed("screencapture", &output))?;
        write_snapshot_metadata(path, window.as_ref())?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let window = active_window_metadata(include_accessibility).await;
        let command = tokio::process::Command::new("gnome-screenshot")
            .args(["-w", "-f"])
            .arg(path);
        let output = run_snapshot_command(command).await?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| snapshot_command_failed("gnome-screenshot", &output))?;
        write_snapshot_metadata(path, window.as_ref())?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let window = active_window_metadata(include_accessibility).await;
        let script = r#"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public static class RemoteAgentCapture {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
}
"@
$window = [RemoteAgentCapture]::GetForegroundWindow()
$rect = New-Object RemoteAgentCapture+RECT
if (-not [RemoteAgentCapture]::GetWindowRect($window, [ref]$rect)) { exit 1 }
$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
if ($width -le 0 -or $height -le 0) { exit 1 }
$bitmap = New-Object Drawing.Bitmap $width, $height
$graphics = [Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
$bitmap.Save($env:REMOTE_AGENT_CAPTURE_PATH, [Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bitmap.Dispose()
"#;
        let command = tokio::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ])
            .env("REMOTE_AGENT_CAPTURE_PATH", path);
        let output = run_snapshot_command(command).await?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| snapshot_command_failed("powershell", &output))?;
        write_snapshot_metadata(path, window.as_ref())?;
        Ok(())
    }
}

/// Plays the user's selected capture feedback without making sound a
/// prerequisite for attaching the image. Desktop environments may omit the
/// optional player; the capture itself remains successful in that case.
pub(crate) async fn play_snapshot_sound(
    sound: agent_core::view::snapshot_capture::SnapshotSound,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let file = match sound {
            agent_core::view::snapshot_capture::SnapshotSound::SoftPop => {
                "/System/Library/Sounds/Pop.aiff"
            }
            agent_core::view::snapshot_capture::SnapshotSound::CameraShutter => {
                "/System/Library/Sounds/Camera Shutter.aiff"
            }
        };
        let output = run_snapshot_command(tokio::process::Command::new("afplay").arg(file)).await?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| snapshot_command_failed("afplay", &output))
    }
    #[cfg(target_os = "linux")]
    {
        let id = match sound {
            agent_core::view::snapshot_capture::SnapshotSound::SoftPop => "message-new-instant",
            agent_core::view::snapshot_capture::SnapshotSound::CameraShutter => "camera-shutter",
        };
        let output = run_snapshot_command(
            tokio::process::Command::new("canberra-gtk-play").args(["-i", id]),
        )
        .await?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| snapshot_command_failed("canberra-gtk-play", &output))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = sound;
        Err("capture sound is unavailable on this Windows build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SNAPSHOT_MAX_OUTPUT_BYTES, decode_snapshot_accessibility, read_snapshot_output,
        valid_snapshot_id,
    };

    #[test]
    fn external_snapshot_ids_cannot_escape_the_queue_directory() {
        assert!(valid_snapshot_id("12345678-1234"));
        assert!(!valid_snapshot_id("ABCDEF"));
        assert!(!valid_snapshot_id("../outside"));
        assert!(!valid_snapshot_id(""));
        assert!(!valid_snapshot_id(&"a".repeat(65)));
    }

    #[test]
    fn snapshot_accessibility_accepts_reference_and_domain_wire_shapes() {
        let reference = serde_json::json!({
            "format": "flat-text",
            "text": "button",
            "truncated": false,
        });
        let domain = serde_json::json!({
            "flat-text": {
                "text": "button",
                "truncated": false,
            },
        });
        assert!(decode_snapshot_accessibility(Some(reference)).is_some());
        assert!(decode_snapshot_accessibility(Some(domain)).is_some());
    }

    #[tokio::test]
    async fn snapshot_command_output_keeps_a_bound_while_draining_the_child() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(4096);
        let writer = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; SNAPSHOT_MAX_OUTPUT_BYTES * 2])
                .await
                .unwrap();
        });
        let output = read_snapshot_output(reader).await.unwrap();
        writer.await.unwrap();
        assert_eq!(output.len(), SNAPSHOT_MAX_OUTPUT_BYTES);
    }
}
