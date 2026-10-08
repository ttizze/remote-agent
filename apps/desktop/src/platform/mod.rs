use agent_core::{
    connection::{Store, StoreOptions},
    state::Snapshot,
};
use agent_transport::transport::{Endpoint, Relays, Ticket};
use host_daemon::local_host::{LocalHost, LocalHostRegistry, LocalHostState};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DesktopHandoffAttempt {
    executable: PathBuf,
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
impl Connections {
    pub(crate) async fn connect(
        self: &Arc<Self>,
        remote: Option<&str>,
        snapshot: Snapshot,
        options: StoreOptions,
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
                Store::connect(endpoint, &ticket, snapshot, options, None).await?,
            ));
        }
        let store = Arc::new(self.connect_local(snapshot, options).await?);
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

    async fn connect_local(
        &self,
        snapshot: Snapshot,
        options: StoreOptions,
    ) -> anyhow::Result<Store> {
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
) -> anyhow::Result<bool> {
    let path = desktop_handoff_attempt_path(directory);
    let parent = path
        .parent()
        .expect("desktop handoff marker has a transaction directory");
    host_daemon::platform::create_state_directory(parent)?;
    let bytes = serde_json::to_vec(&DesktopHandoffAttempt {
        executable: executable.to_owned(),
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
    std::fs::remove_file(path)?;
    Ok(true)
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
    acknowledge_desktop_handoff_at(&location.directory, &std::env::current_exe()?)
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
    if !matches!(location.state, LocalHostState::Stopped) {
        return Ok(false);
    }
    let handoff_attempt = desktop_handoff_attempt_path(&location.directory);
    if handoff_attempt.exists() {
        let pending = std::fs::read(&handoff_attempt)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DesktopHandoffAttempt>(&bytes).ok())
            .is_some_and(|attempt| std::fs::symlink_metadata(attempt.executable).is_ok());
        if pending {
            return Ok(false);
        }
        let _ = std::fs::remove_file(&handoff_attempt);
    }
    let Some(installed) = resolve_installed_desktop_executable(&location.directory)? else {
        return Ok(false);
    };
    let current = std::env::current_exe()?;
    if std::fs::canonicalize(&current).ok() == std::fs::canonicalize(&installed).ok() {
        return Ok(false);
    }
    if !write_desktop_handoff_attempt(&location.directory, &installed)? {
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
        acknowledge_desktop_handoff_at, resolve_installed_desktop_executable,
        resolve_installed_host_executable, write_desktop_handoff_attempt,
    };
    use std::fs;

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
        assert!(write_desktop_handoff_attempt(directory.path(), &expected).expect("write marker"));
        assert!(!write_desktop_handoff_attempt(directory.path(), &current).expect("claim marker"));
        assert!(!acknowledge_desktop_handoff_at(directory.path(), &current).expect("old app"));
        assert!(acknowledge_desktop_handoff_at(directory.path(), &expected).expect("new app"));
        assert!(!super::desktop_handoff_attempt_path(directory.path()).exists());
    }
}

pub(crate) fn choose_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}

/// Captures the desktop into a caller-owned path using the platform's native
/// screen capture utility. The caller feeds the resulting file into the same
/// attachment admission path as a picked image.
pub(crate) fn capture_snapshot(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("screencapture");
        command.args(["-x", "-w", "-t", "png"]).arg(path);
        let status = command
            .status()
            .map_err(|error| format!("screen capture could not start: {error}"))?;
        return status
            .success()
            .then_some(())
            .ok_or_else(|| format!("screen capture exited with {status}"));
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("gnome-screenshot");
        command.args(["-w", "-f"]).arg(path);
        let status = command
            .status()
            .map_err(|error| format!("screen capture could not start: {error}"))?;
        return status
            .success()
            .then_some(())
            .ok_or_else(|| format!("screen capture exited with {status}"));
    }
    #[cfg(target_os = "windows")]
    {
        let _ = path;
        Err("Desktop screenshot capture is unavailable on this Windows build.".into())
    }
}

/// Plays the user's selected capture feedback without making sound a
/// prerequisite for attaching the image. Desktop environments may omit the
/// optional player; the capture itself remains successful in that case.
pub(crate) fn play_snapshot_sound(
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
        Command::new("afplay")
            .arg(file)
            .status()
            .map_err(|error| format!("capture sound could not start: {error}"))?
            .success()
            .then_some(())
            .ok_or_else(|| "capture sound failed".into())
    }
    #[cfg(target_os = "linux")]
    {
        let id = match sound {
            agent_core::view::snapshot_capture::SnapshotSound::SoftPop => "message-new-instant",
            agent_core::view::snapshot_capture::SnapshotSound::CameraShutter => "camera-shutter",
        };
        Command::new("canberra-gtk-play")
            .args(["-i", id])
            .status()
            .map_err(|error| format!("capture sound could not start: {error}"))?
            .success()
            .then_some(())
            .ok_or_else(|| "capture sound failed".into())
    }
    #[cfg(target_os = "windows")]
    {
        let _ = sound;
        Err("capture sound is unavailable on this Windows build".into())
    }
}
