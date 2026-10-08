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
    process::{Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::oneshot,
};

const FRAME_MAX_BYTES: usize = 4 * 1024 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const ENCODER_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CDP_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const ENCODER_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ENCODER_OUTPUT_BYTES: usize = 64 * 1024;
const OUTPUT_FPS: f64 = 30.0;
const MAX_ENCODED_FRAMES: u64 = PREVIEW_RECORDING_MAX_DURATION_SECONDS * 30;
const MAX_ENCODED_INPUT_BYTES: u64 = PREVIEW_RECORDING_MAX_BYTES * 4;

pub(crate) const MIME_TYPE: &str = "video/webm;codecs=vp9";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureTermination {
    Cancellation,
    ExplicitStop,
    Deadline,
    Detached,
}

fn capture_termination_result(termination: CaptureTermination) -> Result<(), String> {
    match termination {
        // All bounded termination signals finalize the frames already handed
        // to the encoder.  Encoder and artifact validation still report real
        // failures after cleanup.
        CaptureTermination::Cancellation
        | CaptureTermination::ExplicitStop
        | CaptureTermination::Deadline
        | CaptureTermination::Detached => Ok(()),
    }
}

enum ScreencastEvent {
    Frame(Vec<u8>, Option<f64>, Option<u64>),
    Detached,
}

pub(crate) struct StartResult {
    pub(crate) started_at: String,
    pub(crate) artifact_path: PathBuf,
    pub(crate) externally_detached: Arc<AtomicBool>,
    pub(crate) startup: oneshot::Receiver<Result<(), String>>,
    pub(crate) task: tokio::task::JoinHandle<Result<PreviewRecordingArtifact, String>>,
}

pub(crate) fn start(
    endpoint: String,
    tab_id: String,
    recording_directory: PathBuf,
    width: u32,
    height: u32,
    protected_paths: &[PathBuf],
    cancel: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
) -> Result<StartResult, String> {
    std::fs::create_dir_all(&recording_directory).map_err(|error| {
        format!("recording storage is unavailable: {error}")
    })?;
    prune_directory(&recording_directory, protected_paths)?;
    let id = format!("browser-recording-{}", uuid::Uuid::new_v4().simple());
    let artifact_path = recording_directory.join(format!("{id}.webm"));
    let started_at = chrono::Utc::now().to_rfc3339();
    let (startup_sender, startup) = oneshot::channel();
    let externally_detached = Arc::new(AtomicBool::new(false));
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
        externally_detached.clone(),
    ));
    Ok(StartResult {
        started_at,
        artifact_path,
        externally_detached,
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
    externally_detached: Arc<AtomicBool>,
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
        externally_detached,
    )
    .await;
    match result {
        Ok(size_bytes) => {
            std::fs::rename(&partial_path, &final_path)
                .map_err(|error| format!("recording save-artifact failed: {error}"))?;
            partial_cleanup.disarm();
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
    startup: oneshot::Sender<Result<(), String>>,
    externally_detached: Arc<AtomicBool>,
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
    let session = match command_with_cancel(
        &mut socket,
        &mut next_id,
        None,
        "Target.attachToTarget",
        json!({"targetId":tab_id,"flatten":true}),
        &cancel,
        &stop,
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
            note_target_detached(&error, &externally_detached);
            notify_startup(&mut startup, Err(error.clone()));
            return Err(error);
        }
    };
    if let Err(error) = command_with_cancel(
        &mut socket,
        &mut next_id,
        Some(&session),
        "Page.enable",
        json!({}),
        &cancel,
        &stop,
    )
    .await
    {
        cleanup_cdp(&mut socket, &mut next_id, &session).await;
        note_target_detached(&error, &externally_detached);
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
    if let Err(error) = command_with_cancel(
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
        &cancel,
        &stop,
    )
    .await
    {
        cleanup_cdp(&mut socket, &mut next_id, &session).await;
        encoder.abort().await;
        note_target_detached(&error, &externally_detached);
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
    let mut detached = false;
    let mut deadline_reached = false;
    let capture_result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break capture_termination_result(CaptureTermination::Cancellation),
            _ = stop.cancelled() => break capture_termination_result(CaptureTermination::ExplicitStop),
            // The fixed reference keeps the bounded desktop copy when the
            // capture reaches its deadline.  Let the encoder finalize it;
            // size and frame limits below still bound the artifact.
            _ = &mut deadline => {
                deadline_reached = true;
                break capture_termination_result(CaptureTermination::Deadline);
            },
            message = next_screencast_frame(&mut socket) => {
                let (frame, timestamp, session_id) = match message {
                    Ok(ScreencastEvent::Frame(frame, timestamp, session_id)) => {
                        (frame, timestamp, session_id)
                    }
                    Ok(ScreencastEvent::Detached) => {
                        externally_detached.store(true, Ordering::Release);
                        detached = true;
                        break capture_termination_result(CaptureTermination::Detached);
                    }
                    Err(error) => break Err(error),
                };
                if frame.len() > FRAME_MAX_BYTES {
                    break Err("recording frame exceeds 4 MiB".to_owned());
                }
                if first_timestamp.is_none()
                    && timestamp.is_some_and(|timestamp| timestamp.is_finite())
                {
                    first_timestamp = timestamp;
                }
                let repeats = frame_repetition_count(first_timestamp, timestamp, encoded_frames);
                if encoded_frames.saturating_add(repeats) > MAX_ENCODED_FRAMES {
                    break Err("recording capture duration exceeds 120000ms".to_owned());
                }
                let added_bytes = (frame.len() as u64).saturating_mul(repeats);
                if encoded_input_bytes.saturating_add(added_bytes) > MAX_ENCODED_INPUT_BYTES {
                    break Err("recording encoder input exceeds its bounded limit".to_owned());
                }
                let mut push_error = None;
                for _ in 0..repeats {
                    match tokio::time::timeout(ENCODER_WRITE_TIMEOUT, encoder.push(&frame)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            push_error = Some(error);
                            break;
                        }
                        Err(_) => {
                            push_error = Some(format!(
                                "recording encoder write timed out after {}ms",
                                ENCODER_WRITE_TIMEOUT.as_millis()
                            ));
                            break;
                        }
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
                    if let Err(error) = send_command_with_cancel(
                        &mut socket,
                        &mut next_id,
                        Some(&session),
                        "Page.screencastFrameAck",
                        json!({"sessionId":session_id}),
                        &cancel,
                        &stop,
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
        // Chrome sends Target.detachedFromTarget when a tab is closed or the
        // renderer crashes.  The connection can no longer accept cleanup
        // commands in that case, but frames already handed to the encoder are
        // still a usable desktop copy.
        if detached || deadline_reached {
            return encoder.finish().await;
        }
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
) -> Result<ScreencastEvent, String> {
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
        if text.len() > FRAME_MAX_BYTES.saturating_mul(2) {
            return Err("recording screencast message exceeds 8 MiB".to_owned());
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("recording screencast response is invalid: {error}"))?;
        if is_detached_event(&value) {
            return Ok(ScreencastEvent::Detached);
        }
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
        return Ok(ScreencastEvent::Frame(frame, timestamp, session_id));
    }
}

fn is_detached_event(value: &Value) -> bool {
    value["method"] == "Target.detachedFromTarget" || value["method"] == "Target.targetCrashed"
}

fn note_target_detached(error: &str, externally_detached: &AtomicBool) {
    if error.contains("target detached") {
        externally_detached.store(true, Ordering::Release);
    }
}

async fn cleanup_cdp(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: &str,
) -> Result<(), String> {
    // Wait for both responses, with a bound per command.  A failed stop must
    // not prevent the detach from being sent, and a failed detach must not be
    // hidden by the stop error.
    let stop = cleanup_command(
        socket,
        next_id,
        Some(session),
        "Page.stopScreencast",
        json!({}),
    )
    .await;
    let detach = cleanup_command(
        socket,
        next_id,
        None,
        "Target.detachFromTarget",
        json!({"sessionId":session}),
    )
    .await;
    combine_cleanup_results(stop, detach)
}

const CDP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

async fn cleanup_command(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<(), String> {
    tokio::time::timeout(
        CDP_CLEANUP_TIMEOUT,
        command(socket, next_id, session, method, params),
    )
    .await
    .map_err(|_| {
        format!(
            "recording {method} cleanup timed out after {}ms",
            CDP_CLEANUP_TIMEOUT.as_millis()
        )
    })?
    .map(|_| ())
}

fn combine_cleanup_results(
    stop: Result<(), String>,
    detach: Result<(), String>,
) -> Result<(), String> {
    match (stop, detach) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(stop), Err(detach)) => Err(format!(
            "{stop}; recording Target.detachFromTarget cleanup also failed: {detach}"
        )),
    }
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
    let elapsed_frames = (timestamp - first) * OUTPUT_FPS;
    // Keep the value below the cast bound.  A finite CDP timestamp can still
    // be large enough that the float-to-u64 cast saturates, and adding one to
    // that result used to overflow before the duration cap was checked.
    if !elapsed_frames.is_finite() || elapsed_frames >= MAX_ENCODED_FRAMES as f64 {
        return MAX_ENCODED_FRAMES.saturating_add(1);
    }
    let target = elapsed_frames.round().max(0.0) as u64;
    target.saturating_add(1).saturating_sub(encoded_frames).max(1)
}

fn notify_startup(
    startup: &mut Option<oneshot::Sender<Result<(), String>>>,
    result: Result<(), String>,
) {
    if let Some(sender) = startup.take() {
        let _ = sender.send(result);
    }
}

fn prune_directory(directory: &Path, protected_paths: &[PathBuf]) -> Result<(), String> {
    const MAX_STORAGE_BYTES: u64 = PREVIEW_RECORDING_MAX_BYTES * 4;
    let mut files = std::fs::read_dir(directory)
        .map_err(|error| format!("recording storage is unavailable: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let path = entry.path();
            metadata.is_file().then_some((path, metadata))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(_, metadata)| metadata.modified().ok());
    let mut total = files.iter().map(|(_, metadata)| metadata.len()).sum::<u64>();
    for (path, metadata) in files {
        let protected = protected_paths
            .iter()
            .any(|protected| protected == &path || partial_path_for(protected) == path);
        if is_partial_artifact(&path) {
            if !protected {
                total = total.saturating_sub(metadata.len());
                let _ = std::fs::remove_file(path);
            }
            continue;
        }
        if total <= MAX_STORAGE_BYTES {
            break;
        }
        if protected {
            continue;
        }
        total = total.saturating_sub(metadata.len());
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn partial_path_for(path: &Path) -> PathBuf {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return path.with_extension("part.webm");
    };
    path.with_file_name(format!("{stem}.part.webm"))
}

fn is_partial_artifact(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".part.webm"))
}

async fn command(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    tokio::time::timeout(
        CDP_COMMAND_TIMEOUT,
        command_inner(socket, next_id, session, method, params),
    )
    .await
    .map_err(|_| {
        format!(
            "recording {method} timed out after {}ms",
            CDP_COMMAND_TIMEOUT.as_millis()
        )
    })?
}

async fn command_inner(
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
        if is_detached_event(&value) {
            return Err(format!("recording {method} target detached"));
        }
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

async fn command_with_cancel(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: Option<&str>,
    method: &str,
    params: Value,
    cancel: &tokio_util::sync::CancellationToken,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<Value, String> {
    tokio::select! {
        result = command(socket, next_id, session, method, params) => result,
        _ = cancel.cancelled() => Err(format!("recording {method} was cancelled")),
        _ = stop.cancelled() => Err(format!("recording {method} was cancelled")),
    }
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
    tokio::time::timeout(
        CDP_COMMAND_TIMEOUT,
        socket.send(Message::Text(request.to_string().into())),
    )
    .await
    .map_err(|_| {
        format!(
            "recording {method} send timed out after {}ms",
            CDP_COMMAND_TIMEOUT.as_millis()
        )
    })?
    .map_err(|error| format!("recording {method} send failed: {error}"))?;
    Ok(id)
}

async fn send_command_with_cancel(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    session: Option<&str>,
    method: &str,
    params: Value,
    cancel: &tokio_util::sync::CancellationToken,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<u64, String> {
    tokio::select! {
        result = send_command(socket, next_id, session, method, params) => result,
        _ = cancel.cancelled() => Err(format!("recording {method} was cancelled")),
        _ = stop.cancelled() => Err(format!("recording {method} was cancelled")),
    }
}

struct Encoder {
    child: Child,
    input: Option<ChildStdin>,
    output: PathBuf,
    executable: PathBuf,
}

fn ffmpeg_executable() -> PathBuf {
    if let Some(path) = std::env::var_os("AGENT_FFMPEG_EXECUTABLE") {
        return PathBuf::from(path);
    }
    let sibling_name = if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let sibling = directory.join(sibling_name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(sibling_name)
}

async fn read_bounded<R>(reader: R) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut reader = reader.take((MAX_ENCODER_OUTPUT_BYTES + 1) as u64);
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .await
        .map_err(|error| format!("recording encoder output could not be read: {error}"))?;
    if output.len() > MAX_ENCODER_OUTPUT_BYTES {
        return Err(format!(
            "recording encoder output exceeds {} bytes",
            MAX_ENCODER_OUTPUT_BYTES
        ));
    }
    Ok(output)
}

async fn wait_for_bounded_output(mut child: Child) -> Result<Output, String> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "recording encoder stdout is unavailable".to_owned())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "recording encoder stderr is unavailable".to_owned())?;
    let (stdout, stderr, status) = tokio::try_join!(
        read_bounded(&mut stdout),
        read_bounded(&mut stderr),
        async {
            child
                .wait()
                .await
                .map_err(|error| format!("recording encoder wait failed: {error}"))
        }
    )?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

async fn run_bounded_command(mut command: Command) -> Result<Output, String> {
    let child = command
        .spawn()
        .map_err(|error| format!("recording encoder process could not start: {error}"))?;
    tokio::time::timeout(ENCODER_COMMAND_TIMEOUT, wait_for_bounded_output(child))
        .await
        .map_err(|_| {
            format!(
                "recording encoder process timed out after {}ms",
                ENCODER_COMMAND_TIMEOUT.as_millis()
            )
        })?
}

impl Encoder {
    async fn start(output: &Path) -> Result<Self, String> {
        let executable = ffmpeg_executable();
        let mut probe = Command::new(&executable);
        probe
            .args(["-hide_banner", "-loglevel", "error", "-encoders"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let encoders = run_bounded_command(probe).await.map_err(|error| {
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
            .stdout(Stdio::piped())
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
        let child = self.child;
        let output_path = self.output;
        let executable = self.executable;
        let output = tokio::time::timeout(FINALIZE_TIMEOUT, wait_for_bounded_output(child))
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
        let size = tokio::fs::metadata(&output_path)
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
        let mut file = tokio::fs::File::open(&output_path)
            .await
            .map_err(|error| format!("recording save-artifact failed: {error}"))?;
        file.read_exact(&mut header)
            .await
            .map_err(|error| format!("recording save-artifact is not a WebM file: {error}"))?;
        if header != [0x1a, 0x45, 0xdf, 0xa3] {
            return Err("recording save-artifact is not a WebM file".into());
        }
        drop(file);
        let mut validation = Command::new(&executable);
        validation
            .args(["-hide_banner", "-loglevel", "info", "-i"])
            .arg(&output_path)
            .args(["-map", "0:v:0", "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let validation = run_bounded_command(validation)
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
        assert_eq!(
            frame_repetition_count(Some(0.0), Some(f64::MAX), 0),
            MAX_ENCODED_FRAMES + 1
        );
    }

    #[test]
    fn cleanup_reports_both_failed_cdp_commands() {
        let error = combine_cleanup_results(
            Err("stop failed".into()),
            Err("detach failed".into()),
        )
        .unwrap_err();
        assert!(error.contains("stop failed"));
        assert!(error.contains("detach failed"));
    }

    #[test]
    fn external_target_close_is_a_capture_termination_event() {
        assert!(is_detached_event(&json!({"method":"Target.detachedFromTarget"})));
        assert!(is_detached_event(&json!({"method":"Target.targetCrashed"})));
        assert!(!is_detached_event(&json!({"method":"Page.screencastFrame"})));
    }

    #[test]
    fn deadline_is_a_normal_finalization_signal() {
        assert!(capture_termination_result(CaptureTermination::Cancellation).is_ok());
        assert!(capture_termination_result(CaptureTermination::ExplicitStop).is_ok());
        assert!(capture_termination_result(CaptureTermination::Deadline).is_ok());
        assert!(capture_termination_result(CaptureTermination::Detached).is_ok());
    }

    #[test]
    fn fake_encoder_keeps_the_completed_copy_when_deadline_fires() {
        let directory = tempfile::tempdir().unwrap();
        let partial = directory.path().join("capture.part.webm");
        let final_path = directory.path().join("capture.webm");
        std::fs::write(&partial, [0x1a, 0x45, 0xdf, 0xa3]).unwrap();
        let mut cleanup = PartialArtifactCleanup::new(partial.clone());

        assert!(capture_termination_result(CaptureTermination::Deadline).is_ok());
        std::fs::rename(&partial, &final_path).unwrap();
        cleanup.disarm();

        assert!(final_path.exists());
        assert!(!partial.exists());
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

    #[test]
    fn storage_pruning_keeps_in_progress_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let completed = directory.path().join("old.webm");
        std::fs::File::create(&completed)
            .unwrap()
            .set_len(PREVIEW_RECORDING_MAX_BYTES * 4 + 1)
            .unwrap();
        let partial = directory.path().join("old.part.webm");
        std::fs::write(&partial, b"partial").unwrap();

        prune_directory(directory.path(), std::slice::from_ref(&partial)).unwrap();

        assert!(!completed.exists());
        assert!(partial.exists());
    }

    #[test]
    fn storage_pruning_removes_stale_partial_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let partial = directory.path().join("stale.part.webm");
        std::fs::write(&partial, b"stale").unwrap();

        prune_directory(directory.path(), &[]).unwrap();

        assert!(!partial.exists());
    }

    #[test]
    fn storage_pruning_keeps_artifacts_still_offered_to_the_client() {
        let directory = tempfile::tempdir().unwrap();
        let offered = directory.path().join("offered.webm");
        std::fs::File::create(&offered)
            .unwrap()
            .set_len(PREVIEW_RECORDING_MAX_BYTES * 2)
            .unwrap();
        let old = directory.path().join("old.webm");
        std::fs::File::create(&old)
            .unwrap()
            .set_len(PREVIEW_RECORDING_MAX_BYTES * 3)
            .unwrap();

        prune_directory(directory.path(), std::slice::from_ref(&offered)).unwrap();

        assert!(offered.exists());
        assert!(!old.exists());
    }
}
