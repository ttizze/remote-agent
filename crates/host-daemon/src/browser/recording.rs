//! Host-side Preview recording.
//!
//! Recording uses a second authenticated CDP connection so the shared browser
//! socket remains available for navigation and input while the user records.
//! CDP screencast JPEG frames are bounded before they reach the pinned ffmpeg
//! image pipe.  The completed WebM is an ordinary Host artifact and can be
//! downloaded by a desktop client or consumed by the browser MCP bridge.

use agent_protocol::preview::{
    PREVIEW_RECORDING_MAX_BYTES, PREVIEW_RECORDING_MAX_DURATION_SECONDS,
    PreviewRecordingArtifact,
};
use async_tungstenite::{WebSocketStream, tokio::ConnectStream, tungstenite::Message};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, ChildStdin, Command},
    sync::oneshot,
};

const FRAME_MAX_BYTES: usize = 4 * 1024 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const MIME_TYPE: &str = "video/webm";

pub(crate) struct StartResult {
    pub(crate) started_at: String,
    pub(crate) startup: oneshot::Receiver<Result<(), String>>,
    pub(crate) task: tokio::task::JoinHandle<Result<PreviewRecordingArtifact, String>>,
}

pub(crate) fn start(
    endpoint: String,
    tab_id: String,
    recording_directory: PathBuf,
    width: u32,
    height: u32,
    cancel: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
) -> Result<StartResult, String> {
    std::fs::create_dir_all(&recording_directory).map_err(|error| {
        format!("recording storage is unavailable: {error}")
    })?;
    prune_directory(&recording_directory)?;
    let id = format!("browser-recording-{}", uuid::Uuid::new_v4().simple());
    let started_at = chrono::Utc::now().to_rfc3339();
    let (startup_sender, startup) = oneshot::channel();
    let task = tokio::spawn(run(
        endpoint,
        tab_id,
        recording_directory,
        id,
        width,
        height,
        cancel,
        stop,
        startup_sender,
    ));
    Ok(StartResult {
        started_at,
        startup,
        task,
    })
}

pub(crate) async fn await_startup(
    startup: oneshot::Receiver<Result<(), String>>,
) -> Result<(), String> {
    tokio::time::timeout(STARTUP_TIMEOUT, startup)
        .await
        .map_err(|_| "recording startup timed out after 5000ms".to_owned())?
        .map_err(|_| "recording startup was cancelled".to_owned())?
}

async fn run(
    endpoint: String,
    tab_id: String,
    recording_directory: PathBuf,
    id: String,
    width: u32,
    height: u32,
    cancel: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
    startup: oneshot::Sender<Result<(), String>>,
) -> Result<PreviewRecordingArtifact, String> {
    let final_path = recording_directory.join(format!("{id}.webm"));
    let partial_path = recording_directory.join(format!("{id}.part.webm"));
    let result = run_capture(
        &endpoint,
        &tab_id,
        &partial_path,
        width,
        height,
        cancel,
        stop,
        startup,
    )
    .await;
    match result {
        Ok(size_bytes) => {
            std::fs::rename(&partial_path, &final_path)
                .map_err(|error| format!("recording save-artifact failed: {error}"))?;
            prune_directory(&recording_directory)?;
            Ok(PreviewRecordingArtifact {
                id,
                tab_id,
                path: final_path.to_string_lossy().into_owned(),
                mime_type: MIME_TYPE.into(),
                size_bytes,
                created_at: chrono::Utc::now().to_rfc3339(),
            })
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial_path);
            Err(error)
        }
    }
}

async fn run_capture(
    endpoint: &str,
    tab_id: &str,
    output: &Path,
    width: u32,
    height: u32,
    cancel: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
    mut startup: oneshot::Sender<Result<(), String>>,
) -> Result<u64, String> {
    let mut startup = Some(startup);
    let (mut socket, _) = match async_tungstenite::tokio::connect_async(endpoint).await {
        Ok(socket) => socket,
        Err(error) => {
            let error = format!("recording capture-media-stream failed: {error}");
            notify_startup(&mut startup, Err(error.clone()));
            return Err(error);
        }
    };
    let mut next_id = 0;
    let attach = command(
        &mut socket,
        &mut next_id,
        None,
        "Target.attachToTarget",
        json!({"targetId":tab_id,"flatten":true}),
    )
    .await;
    let session = match attach {
        Ok(value) => match value["sessionId"].as_str().map(str::to_owned) {
            Some(session) => session,
            None => {
                let error = "recording start-screencast returned no session".to_owned();
                notify_startup(&mut startup, Err(error.clone()));
                return Err(error);
            }
        },
        Err(error) => {
            notify_startup(&mut startup, Err(error.clone()));
            return Err(error);
        }
    };
    if let Err(error) = command(
        &mut socket,
        &mut next_id,
        Some(&session),
        "Page.enable",
        json!({}),
    )
    .await
    {
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    let mut encoder = match Encoder::start(output).await {
        Ok(encoder) => encoder,
        Err(error) => {
            notify_startup(&mut startup, Err(error.clone()));
            return Err(error);
        }
    };
    if let Err(error) = command(
        &mut socket,
        &mut next_id,
        Some(&session),
        "Page.startScreencast",
        json!({
            "format":"jpeg",
            "quality":80,
            "maxWidth":width,
            "maxHeight":height,
            "everyNthFrame":1
        }),
    )
    .await
    {
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    if cancel.is_cancelled() || stop.is_cancelled() {
        let error = "recording start was cancelled".to_owned();
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    notify_startup(&mut startup, Ok(()));
    let deadline = tokio::time::sleep(Duration::from_secs(
        PREVIEW_RECORDING_MAX_DURATION_SECONDS,
    ));
    tokio::pin!(deadline);
    let mut frames = 0u64;
    let capture_result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            _ = stop.cancelled() => break Ok(()),
            _ = &mut deadline => break Err("recording capture deadline exceeded after 120000ms".to_owned()),
            message = socket.next() => {
                let Some(message) = message else {
                    break Err("recording screencast connection closed".to_owned());
                };
                let message = message.map_err(|error| format!("recording screencast failed: {error}"))?;
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => break Err("recording screencast connection closed".to_owned()),
                    _ => continue,
                };
                let value: Value = serde_json::from_str(&text)
                    .map_err(|error| format!("recording screencast response is invalid: {error}"))?;
                if value["method"] == "Page.screencastFrame" {
                    let data = value["params"]["data"]
                        .as_str()
                        .ok_or_else(|| "recording screencast frame has no data".to_owned())?;
                    let frame = STANDARD.decode(data)
                        .map_err(|error| format!("recording screencast frame is invalid: {error}"))?;
                    if frame.len() > FRAME_MAX_BYTES {
                        break Err("recording frame exceeds 4 MiB".to_owned());
                    }
                    encoder.push(&frame).await?;
                    if tokio::fs::metadata(output)
                        .await
                        .is_ok_and(|metadata| metadata.len() > PREVIEW_RECORDING_MAX_BYTES)
                    {
                        break Err(format!(
                            "recording artifact exceeds {} MiB",
                            PREVIEW_RECORDING_MAX_BYTES / (1024 * 1024)
                        ));
                    }
                    frames = frames.saturating_add(1);
                    if let Some(session_id) = value["params"]["sessionId"].as_u64() {
                        send_command(
                            &mut socket,
                            &mut next_id,
                            Some(&session),
                            "Page.screencastFrameAck",
                            json!({"sessionId":session_id}),
                        ).await?;
                    }
                }
            }
        }
    };
    let _ = send_command(
        &mut socket,
        &mut next_id,
        Some(&session),
        "Page.stopScreencast",
        json!({}),
    )
    .await;
    let _ = send_command(
        &mut socket,
        &mut next_id,
        None,
        "Target.detachFromTarget",
        json!({"sessionId":session}),
    )
    .await;
    if let Err(error) = capture_result {
        return Err(error);
    }
    if frames == 0 {
        return Err("recording produced no browser frames".to_owned());
    }
    encoder.finish().await
}

fn notify_startup(
    startup: &mut Option<oneshot::Sender<Result<(), String>>>,
    result: Result<(), String>,
) {
    if let Some(sender) = startup.take() {
        let _ = sender.send(result);
    }
}

fn prune_directory(directory: &Path) -> Result<(), String> {
    const MAX_STORAGE_BYTES: u64 = PREVIEW_RECORDING_MAX_BYTES * 4;
    let mut files = std::fs::read_dir(directory)
        .map_err(|error| format!("recording storage is unavailable: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then_some((entry.path(), metadata))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(_, metadata)| metadata.modified().ok());
    let mut total = files.iter().map(|(_, metadata)| metadata.len()).sum::<u64>();
    for (path, metadata) in files {
        if total <= MAX_STORAGE_BYTES {
            break;
        }
        total = total.saturating_sub(metadata.len());
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

async fn command(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = send_command(socket, next_id, session, method, params).await?;
    while let Some(message) = socket.next().await {
        let message = message.map_err(|error| format!("recording {method} failed: {error}"))?;
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("recording {method} response is invalid: {error}"))?;
        if value["id"].as_u64() != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(format!("recording {method} failed: {error}"));
        }
        return Ok(value["result"].clone());
    }
    Err(format!("recording {method} connection closed"))
}

async fn send_command(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<u64, String> {
    *next_id = next_id.saturating_add(1);
    let id = *next_id;
    let mut request = json!({"id":id,"method":method,"params":params});
    if let Some(session) = session {
        request["sessionId"] = session.into();
    }
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(|error| format!("recording {method} send failed: {error}"))?;
    Ok(id)
}

struct Encoder {
    child: Child,
    input: Option<ChildStdin>,
    output: PathBuf,
}

impl Encoder {
    async fn start(output: &Path) -> Result<Self, String> {
        let executable = std::env::var_os("BEX_FFMPEG_EXECUTABLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let mut child = Command::new(&executable)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "image2pipe",
                "-framerate",
                "30",
                "-vcodec",
                "mjpeg",
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "libvpx-vp9",
                "-deadline",
                "realtime",
                "-cpu-used",
                "8",
                "-b:v",
                "2M",
                "-fs",
            ])
            .arg(PREVIEW_RECORDING_MAX_BYTES.to_string())
            .args([
                "-f",
                "webm",
            ])
            .arg(output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("recording initialize-media-recorder failed: {error}"))?;
        let input = child.stdin.take().ok_or_else(|| {
            "recording initialize-media-recorder failed: ffmpeg stdin unavailable".to_owned()
        })?;
        Ok(Self {
            child,
            input: Some(input),
            output: output.to_owned(),
        })
    }

    async fn push(&mut self, frame: &[u8]) -> Result<(), String> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| "recording encoder is already closed".to_owned())?;
        input
            .write_all(frame)
            .await
            .map_err(|error| format!("recording encoder write failed: {error}"))
    }

    async fn finish(mut self) -> Result<u64, String> {
        drop(self.input.take());
        let output = tokio::time::timeout(FINALIZE_TIMEOUT, self.child.wait_with_output())
            .await
            .map_err(|_| "recording stop-media-recorder timed out after 10000ms".to_owned())?
            .map_err(|error| format!("recording stop-media-recorder failed: {error}"))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(1024)
                .collect::<String>();
            return Err(format!("recording save-artifact failed: {detail}"));
        }
        let size = tokio::fs::metadata(&self.output)
            .await
            .map_err(|error| format!("recording save-artifact failed: {error}"))?
            .len();
        if size == 0 {
            return Err("recording save-artifact produced an empty artifact".to_owned());
        }
        if size > PREVIEW_RECORDING_MAX_BYTES {
            return Err(format!(
                "recording artifact exceeds {} MiB",
                PREVIEW_RECORDING_MAX_BYTES / (1024 * 1024)
            ));
        }
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_paths_use_a_host_generated_id_and_webm_extension() {
        let id = format!("browser-recording-{}", uuid::Uuid::new_v4().simple());
        assert!(id.starts_with("browser-recording-"));
        assert!(id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
        assert_eq!(format!("{id}.webm").ends_with(".webm"), true);
    }

    #[test]
    fn recording_limits_are_bounded() {
        assert_eq!(PREVIEW_RECORDING_MAX_BYTES, 50 * 1024 * 1024);
        assert_eq!(PREVIEW_RECORDING_MAX_DURATION_SECONDS, 120);
    }
}
