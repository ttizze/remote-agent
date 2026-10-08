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
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::oneshot,
};

const FRAME_MAX_BYTES: usize = 4 * 1024 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_FPS: f64 = 30.0;
const MAX_ENCODED_FRAMES: u64 = PREVIEW_RECORDING_MAX_DURATION_SECONDS * 30;
const MAX_ENCODED_INPUT_BYTES: u64 = PREVIEW_RECORDING_MAX_BYTES * 4;

pub(crate) const MIME_TYPE: &str = "video/webm;codecs=vp9";

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
    let mut partial_cleanup = PartialArtifactCleanup::new(partial_path.clone());
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
            partial_cleanup.disarm();
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
            Err(error)
        }
    }
}

struct PartialArtifactCleanup {
    path: PathBuf,
    armed: bool,
}

impl PartialArtifactCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartialArtifactCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
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
    let session = match command(
        &mut socket,
        &mut next_id,
        None,
        "Target.attachToTarget",
        json!({"targetId":tab_id,"flatten":true}),
    )
    .await
    {
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
        cleanup_cdp(&mut socket, &mut next_id, &session).await;
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    let mut encoder = match Encoder::start(output).await {
        Ok(encoder) => encoder,
        Err(error) => {
            cleanup_cdp(&mut socket, &mut next_id, &session).await;
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
        cleanup_cdp(&mut socket, &mut next_id, &session).await;
        encoder.abort().await;
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    if cancel.is_cancelled() || stop.is_cancelled() {
        let error = "recording start was cancelled".to_owned();
        cleanup_cdp(&mut socket, &mut next_id, &session).await;
        encoder.abort().await;
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    notify_startup(&mut startup, Ok(()));
    let deadline = tokio::time::sleep(Duration::from_secs(
        PREVIEW_RECORDING_MAX_DURATION_SECONDS,
    ));
    tokio::pin!(deadline);
    let mut frames = 0u64;
    let mut encoded_frames = 0u64;
    let mut encoded_input_bytes = 0u64;
    let mut first_timestamp = None;
    let capture_result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            _ = stop.cancelled() => break Ok(()),
            _ = &mut deadline => break Err("recording capture deadline exceeded after 120000ms".to_owned()),
            message = next_screencast_frame(&mut socket) => {
                let (frame, timestamp, session_id) = match message {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(error) => break Err(error),
                };
                if frame.len() > FRAME_MAX_BYTES {
                    break Err("recording frame exceeds 4 MiB".to_owned());
                }
                let repeats = frame_repetition_count(first_timestamp, timestamp, encoded_frames);
                if encoded_frames.saturating_add(repeats) > MAX_ENCODED_FRAMES {
                    break Err("recording capture duration exceeds 120000ms".to_owned());
                }
                let added_bytes = (frame.len() as u64).saturating_mul(repeats);
                if encoded_input_bytes.saturating_add(added_bytes) > MAX_ENCODED_INPUT_BYTES {
                    break Err("recording encoder input exceeds its bounded limit".to_owned());
                }
                if first_timestamp.is_none() {
                    first_timestamp = timestamp;
                }
                let mut push_error = None;
                for _ in 0..repeats {
                    if let Err(error) = encoder.push(&frame).await {
                        push_error = Some(error);
                        break;
                    }
                }
                if let Some(error) = push_error {
                    break Err(error);
                }
                if tokio::fs::metadata(output)
                    .await
                    .is_ok_and(|metadata| metadata.len() > PREVIEW_RECORDING_MAX_BYTES)
                {
                    break Err(format!(
                        "recording artifact exceeds {} MiB",
                        PREVIEW_RECORDING_MAX_BYTES / (1024 * 1024)
                    ));
                }
                encoded_frames = encoded_frames.saturating_add(repeats);
                encoded_input_bytes = encoded_input_bytes.saturating_add(added_bytes);
                frames = frames.saturating_add(1);
                if let Some(session_id) = session_id {
                    if let Err(error) = send_command(
                        &mut socket,
                        &mut next_id,
                        Some(&session),
                        "Page.screencastFrameAck",
                        json!({"sessionId":session_id}),
                    ).await {
                        break Err(error);
                    }
                }
            }
        }
    };
    let cleanup_result = cleanup_cdp(&mut socket, &mut next_id, &session).await;
    if let Err(error) = capture_result {
        encoder.abort().await;
        return Err(error);
    }
    if let Err(error) = cleanup_result {
        encoder.abort().await;
        return Err(error);
    }
    if frames == 0 {
        encoder.abort().await;
        return Err("recording produced no browser frames".to_owned());
    }
    encoder.finish().await
}

async fn next_screencast_frame(
    socket: &mut WebSocketStream<ConnectStream>,
) -> Result<Option<(Vec<u8>, Option<f64>, Option<u64>)>, String> {
    loop {
        let Some(message) = socket.next().await else {
            return Err("recording screencast connection closed".to_owned());
        };
        let message = message.map_err(|error| format!("recording screencast failed: {error}"))?;
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => return Err("recording screencast connection closed".to_owned()),
            _ => continue,
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("recording screencast response is invalid: {error}"))?;
        if value["method"] != "Page.screencastFrame" {
            continue;
        }
        let data = value["params"]["data"]
            .as_str()
            .ok_or_else(|| "recording screencast frame has no data".to_owned())?;
        let frame = STANDARD
            .decode(data)
            .map_err(|error| format!("recording screencast frame is invalid: {error}"))?;
        let timestamp = value["params"]["metadata"]["timestamp"].as_f64();
        let session_id = value["params"]["sessionId"].as_u64();
        return Ok(Some((frame, timestamp, session_id)));
    }
}

async fn cleanup_cdp(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: &str,
) -> Result<(), String> {
    let stop = send_command(
        socket,
        next_id,
        Some(session),
        "Page.stopScreencast",
        json!({}),
    )
    .await;
    let detach = send_command(
        socket,
        next_id,
        None,
        "Target.detachFromTarget",
        json!({"sessionId":session}),
    )
    .await;
    stop.and(detach).map(|_| ())
}

fn frame_repetition_count(
    first_timestamp: Option<f64>,
    timestamp: Option<f64>,
    encoded_frames: u64,
) -> u64 {
    let Some(first) = first_timestamp else { return 1; };
    let Some(timestamp) = timestamp.filter(|timestamp| timestamp.is_finite() && *timestamp >= first) else {
        return 1;
    };
    let target = ((timestamp - first) * OUTPUT_FPS).round() as u64 + 1;
    target.saturating_sub(encoded_frames).max(1)
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
    executable: PathBuf,
}

impl Encoder {
    async fn start(output: &Path) -> Result<Self, String> {
        let executable = std::env::var_os("BEX_FFMPEG_EXECUTABLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let encoders = Command::new(&executable)
            .args(["-hide_banner", "-loglevel", "error", "-encoders"])
            .output()
            .await
            .map_err(|error| {
                format!(
                    "recording initialize-media-recorder failed: ffmpeg is unavailable: {error}"
                )
            })?;
        let encoder_list = format!(
            "{}{}",
            String::from_utf8_lossy(&encoders.stdout),
            String::from_utf8_lossy(&encoders.stderr)
        );
        if !encoders.status.success() || !encoder_list.contains("libvpx-vp9") {
            return Err(
                "recording initialize-media-recorder failed: ffmpeg lacks the libvpx-vp9 encoder"
                    .into(),
            );
        }
        let mut child = Command::new(&executable)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "image2pipe",
                "-framerate",
            ])
            .arg(OUTPUT_FPS.to_string())
            .args([
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
            .args(["-f", "webm"])
            .arg(output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("recording initialize-media-recorder failed: {error}"))?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("recording initialize-media-recorder failed: {error}"))?
        {
            return Err(format!(
                "recording initialize-media-recorder failed: ffmpeg exited with {status}"
            ));
        }
        let input = child.stdin.take().ok_or_else(|| {
            "recording initialize-media-recorder failed: ffmpeg stdin unavailable".to_owned()
        })?;
        Ok(Self {
            child,
            input: Some(input),
            output: output.to_owned(),
            executable,
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
        let mut header = [0u8; 4];
        let mut file = tokio::fs::File::open(&self.output)
            .await
            .map_err(|error| format!("recording save-artifact failed: {error}"))?;
        file.read_exact(&mut header)
            .await
            .map_err(|error| format!("recording save-artifact is not a WebM file: {error}"))?;
        if header != [0x1a, 0x45, 0xdf, 0xa3] {
            return Err("recording save-artifact is not a WebM file".into());
        }
        drop(file);
        let validation = Command::new(&self.executable)
            .args(["-hide_banner", "-loglevel", "info", "-i"])
            .arg(&self.output)
            .args(["-map", "0:v:0", "-f", "null", "-"])
            .output()
            .await
            .map_err(|error| format!("recording save-artifact validation failed: {error}"))?;
        let validation_detail = format!(
            "{}{}",
            String::from_utf8_lossy(&validation.stdout),
            String::from_utf8_lossy(&validation.stderr)
        );
        if !validation.status.success() || !validation_detail.to_ascii_lowercase().contains("vp9") {
            let detail = validation_detail
                .chars()
                .take(1024)
                .collect::<String>();
            return Err(format!(
                "recording save-artifact is not a decodable WebM video: {detail}"
            ));
        }
        Ok(size)
    }

    async fn abort(mut self) {
        drop(self.input.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill().await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = tokio::fs::remove_file(self.output).await;
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
        assert_eq!(MIME_TYPE, "video/webm;codecs=vp9");
    }

    #[test]
    fn screencast_timestamps_expand_gaps_into_encoder_frames() {
        assert_eq!(frame_repetition_count(None, Some(10.0), 0), 1);
        assert_eq!(frame_repetition_count(Some(10.0), Some(10.1), 1), 3);
        assert_eq!(frame_repetition_count(Some(10.0), Some(10.1), 4), 1);
        assert_eq!(frame_repetition_count(Some(10.0), Some(9.0), 1), 1);
        assert_eq!(frame_repetition_count(Some(10.0), None, 1), 1);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeCdpPhase {
        Connected,
        Attached,
        Encoding,
        Screencasting,
        Stopping,
        Finished,
    }

    fn fake_cleanup_actions(phase: FakeCdpPhase) -> (bool, bool, bool) {
        (
            matches!(
                phase,
                FakeCdpPhase::Encoding
                    | FakeCdpPhase::Screencasting
                    | FakeCdpPhase::Stopping
            ),
            matches!(
                phase,
                FakeCdpPhase::Attached
                    | FakeCdpPhase::Encoding
                    | FakeCdpPhase::Screencasting
                    | FakeCdpPhase::Stopping
            ),
            matches!(
                phase,
                FakeCdpPhase::Encoding
                    | FakeCdpPhase::Screencasting
                    | FakeCdpPhase::Stopping
            ),
        )
    }

    #[test]
    fn fake_cdp_lifecycle_always_detaches_and_aborts_started_encoders() {
        assert_eq!(fake_cleanup_actions(FakeCdpPhase::Connected), (false, false, false));
        assert_eq!(fake_cleanup_actions(FakeCdpPhase::Attached), (false, true, false));
        assert_eq!(fake_cleanup_actions(FakeCdpPhase::Encoding), (true, true, true));
        assert_eq!(fake_cleanup_actions(FakeCdpPhase::Screencasting), (true, true, true));
        assert_eq!(fake_cleanup_actions(FakeCdpPhase::Stopping), (true, true, true));
        assert_eq!(fake_cleanup_actions(FakeCdpPhase::Finished), (false, false, false));
    }

    #[test]
    fn aborted_capture_removes_its_partial_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.part.webm");
        std::fs::write(&path, b"partial").unwrap();
        {
            let _cleanup = PartialArtifactCleanup::new(path.clone());
        }
        assert!(!path.exists());

        std::fs::write(&path, b"final").unwrap();
        {
            let mut cleanup = PartialArtifactCleanup::new(path.clone());
            cleanup.disarm();
        }
        assert!(path.exists());
    }
}
