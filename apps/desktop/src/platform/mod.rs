use agent_core::{
    connection::{Store, StoreOptions},
    state::Snapshot,
};
use agent_transport::transport::{Endpoint, Relays, Ticket};
use host_daemon::local_host::{LocalHost, LocalHostRegistry, LocalHostState};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
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
fn active_window_metadata(include_accessibility: bool) -> Option<agent_domain::CapturedWindow> {
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
        let output = Command::new("osascript")
            .args(["-e", &script])
            .output()
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
        let id = Command::new("xdotool")
            .arg("getactivewindow")
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        let id = String::from_utf8_lossy(&id.stdout).trim().to_owned();
        if id.is_empty() {
            return None;
        }
        let title = Command::new("xdotool")
            .args(["getwindowname", &id])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default();
        let app_name = Command::new("xprop")
            .args(["-id", &id, "WM_CLASS"])
            .output()
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
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        let mut lines = String::from_utf8_lossy(&output.stdout).lines().map(str::trim);
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

pub(crate) fn snapshot_permission_granted(include_accessibility: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        !include_accessibility
            || Command::new("osascript")
                .args([
                    "-e",
                    "tell application \"System Events\" to get name of first application process whose frontmost is true",
                ])
                .output()
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
pub(crate) fn capture_snapshot(
    path: &std::path::Path,
    include_accessibility: bool,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let window = active_window_metadata(include_accessibility);
        let mut command = Command::new("screencapture");
        command.args(["-x", "-w", "-t", "png"]).arg(path);
        let status = command
            .status()
            .map_err(|error| format!("screen capture could not start: {error}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("screen capture exited with {status}"))?;
        let _ = write_snapshot_metadata(path, window.as_ref());
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let window = active_window_metadata(include_accessibility);
        let mut command = Command::new("gnome-screenshot");
        command.args(["-w", "-f"]).arg(path);
        let status = command
            .status()
            .map_err(|error| format!("screen capture could not start: {error}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("screen capture exited with {status}"))?;
        let _ = write_snapshot_metadata(path, window.as_ref());
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let window = active_window_metadata(include_accessibility);
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
        let status = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
            .env("REMOTE_AGENT_CAPTURE_PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .map_err(|error| format!("screen capture could not start: {error}"))?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("screen capture exited with {status}"))?;
        let _ = write_snapshot_metadata(path, window.as_ref());
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::{decode_snapshot_accessibility, valid_snapshot_id};

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
}
