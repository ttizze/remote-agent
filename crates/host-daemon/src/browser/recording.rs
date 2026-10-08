//! Host-side Preview recording.
//!
//! Recording uses a second authenticated CDP connection so the shared browser
//! socket remains available for navigation and input while the user records.
//! CDP screencast JPEG frames are bounded before they reach the pinned ffmpeg
//! image pipe.  The completed WebM is an ordinary Host artifact and can be
//! downloaded by a desktop client or consumed by the browser MCP bridge.

use agent_protocol::preview::{
    PREVIEW_RECORDING_MAX_BYTES, PREVIEW_RECORDING_MAX_DURATION_SECONDS, PreviewRecordingArtifact,
};
use async_tungstenite::{WebSocketStream, tokio::ConnectStream, tungstenite::Message};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
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
const MAX_ENCODED_INPUT_BYTES: u64 = PREVIEW_RECORDING_MAX_BYTES * 4;

pub(crate) const MIME_TYPE: &str = "video/webm;codecs=vp9";

#[derive(Clone, Debug)]
pub(crate) enum InputEvent {
    Key {
        label: String,
        down: bool,
    },
    Pointer {
        phase: PointerPhase,
        x: f64,
        y: f64,
        width: u32,
        height: u32,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PointerPhase {
    Down,
    Move,
    Up,
    Click,
}

#[derive(Clone, Debug)]
struct KeyOverlay {
    label: String,
    expires_at: Option<Instant>,
}

#[derive(Clone, Debug)]
struct PointerOverlay {
    x: f64,
    y: f64,
    width: u32,
    height: u32,
    held: bool,
    released_at: Option<Instant>,
}

#[derive(Clone, Default, Debug)]
pub(super) struct OverlayState {
    key: Option<KeyOverlay>,
    pointer: Option<PointerOverlay>,
}

pub(crate) type OverlayHandle = Arc<Mutex<OverlayState>>;

pub(crate) fn new_overlay() -> OverlayHandle {
    Arc::new(Mutex::new(OverlayState::default()))
}

fn apply_pointer(
    state: &mut OverlayState,
    phase: PointerPhase,
    x: f64,
    y: f64,
    width: u32,
    height: u32,
    now: Instant,
) {
    match phase {
        PointerPhase::Down => {
            state.pointer = Some(PointerOverlay {
                x,
                y,
                width,
                height,
                held: true,
                released_at: None,
            });
        }
        PointerPhase::Move => {
            if let Some(pointer) = state.pointer.as_mut() {
                pointer.x = x;
                pointer.y = y;
                pointer.width = width;
                pointer.height = height;
            }
        }
        PointerPhase::Up => {
            if let Some(pointer) = state.pointer.as_mut() {
                pointer.x = x;
                pointer.y = y;
                pointer.width = width;
                pointer.height = height;
                pointer.held = false;
                pointer.released_at = Some(now);
            }
        }
        PointerPhase::Click => unreachable!("clicks are expanded into pointer phases"),
    }
}

pub(crate) fn apply_input(overlay: &OverlayHandle, event: InputEvent) {
    let Ok(mut state) = overlay.lock() else {
        return;
    };
    let now = Instant::now();
    match event {
        InputEvent::Key { label, down } => {
            state.key = (!label.is_empty()).then_some(KeyOverlay {
                label,
                expires_at: (!down).then_some(now + Duration::from_millis(900)),
            });
        }
        InputEvent::Pointer {
            phase: PointerPhase::Click,
            x,
            y,
            width,
            height,
        } => {
            // A browser click is the complete pointer lifecycle. Feeding the
            // same phases used by a future drag source keeps the overlay
            // state machine identical for clicks and streamed pointer input.
            for phase in [PointerPhase::Down, PointerPhase::Move, PointerPhase::Up] {
                apply_pointer(&mut state, phase, x, y, width, height, now);
            }
        }
        InputEvent::Pointer {
            phase,
            x,
            y,
            width,
            height,
        } => apply_pointer(&mut state, phase, x, y, width, height, now),
    }
}

fn decorate_jpeg(
    frame: &[u8],
    overlay: &OverlayHandle,
    options: agent_protocol::preview::PreviewRecordingOptions,
) -> Result<Vec<u8>, String> {
    if !options.show_key_presses && !options.show_mouse_presses {
        return Ok(frame.to_vec());
    }
    let image = image::load_from_memory_with_format(frame, image::ImageFormat::Jpeg)
        .map_err(|error| format!("recording overlay could not decode browser frame: {error}"))?;
    let mut image = image.to_rgb8();
    let width = image.width();
    let height = image.height();
    let now = Instant::now();
    let state = overlay
        .lock()
        .map_err(|_| "recording overlay state is unavailable".to_owned())?
        .clone();
    if options.show_mouse_presses
        && let Some(pointer) = state.pointer
        && (pointer.held
            || pointer
                .released_at
                .is_some_and(|released| now.duration_since(released) < Duration::from_millis(600)))
    {
        let progress = pointer
            .released_at
            .map(|released| (now.duration_since(released).as_secs_f32() / 0.6).min(1.0))
            .unwrap_or(0.0);
        let cx = (pointer.x / f64::from(pointer.width.max(1)) * f64::from(width)) as i32;
        let cy = (pointer.y / f64::from(pointer.height.max(1)) * f64::from(height)) as i32;
        let radius = (20.0 * (1.0 + f64::from(progress) * 0.5) * f64::from(width)
            / f64::from(pointer.width.max(1))) as i32;
        draw_ring(&mut image, cx, cy, radius.max(2), [88, 176, 255]);
    }
    if options.show_key_presses
        && let Some(key) = state.key
        && (key.expires_at.is_none() || key.expires_at.is_some_and(|expires| now < expires))
    {
        draw_key_badge(&mut image, &key.label);
    }
    let mut output = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 80)
        .encode_image(&image)
        .map_err(|error| format!("recording overlay could not encode browser frame: {error}"))?;
    Ok(output)
}

fn draw_ring(image: &mut image::RgbImage, cx: i32, cy: i32, radius: i32, color: [u8; 3]) {
    let outer = radius * radius;
    let inner = (radius - 2).max(0) * (radius - 2).max(0);
    for y in (cy - radius - 1).max(0)..=(cy + radius + 1).min(image.height() as i32 - 1) {
        for x in (cx - radius - 1).max(0)..=(cx + radius + 1).min(image.width() as i32 - 1) {
            let distance = (x - cx) * (x - cx) + (y - cy) * (y - cy);
            if inner < distance && distance <= outer {
                image.put_pixel(x as u32, y as u32, image::Rgb(color));
            }
        }
    }
}

fn draw_key_badge(image: &mut image::RgbImage, label: &str) {
    let scale = (image.width().min(image.height()) / 360).max(1) as i32;
    let glyph_width = 5 * scale;
    let spacing = scale;
    let text_width = label.chars().count() as i32 * (glyph_width + spacing);
    let badge_width = (text_width + 18 * scale).min(image.width() as i32);
    let badge_height = 13 * scale;
    let left = ((image.width() as i32 - badge_width) / 2).max(0);
    let top = (image.height() as i32 - badge_height - 10 * scale).max(0);
    for y in top..(top + badge_height).min(image.height() as i32) {
        for x in left..(left + badge_width).min(image.width() as i32) {
            image.put_pixel(x as u32, y as u32, image::Rgb([32, 32, 34]));
        }
    }
    let mut x = left + (badge_width - text_width) / 2;
    for character in label.chars() {
        draw_glyph(image, x, top + 3 * scale, character, scale);
        x += glyph_width + spacing;
    }
}

fn draw_glyph(image: &mut image::RgbImage, left: i32, top: i32, character: char, scale: i32) {
    let pattern = glyph_pattern(character);
    for (row, bits) in pattern.iter().enumerate() {
        for column in 0..5 {
            if bits & (1 << (4 - column)) != 0 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = left + column * scale + dx;
                        let y = top + row as i32 * scale + dy;
                        if x >= 0 && y >= 0 && x < image.width() as i32 && y < image.height() as i32
                        {
                            image.put_pixel(x as u32, y as u32, image::Rgb([255, 255, 255]));
                        }
                    }
                }
            }
        }
    }
}

fn glyph_pattern(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b11100,
        ],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        _ => [
            0b11111, 0b10001, 0b10101, 0b10101, 0b10101, 0b10001, 0b11111,
        ],
    }
}

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
    pub(crate) recording_id: String,
    pub(crate) started_at: String,
    pub(crate) artifact_path: PathBuf,
    pub(crate) externally_detached: Arc<AtomicBool>,
    pub(crate) startup: oneshot::Receiver<Result<(), String>>,
    pub(crate) task: tokio::task::JoinHandle<Result<PreviewRecordingArtifact, String>>,
    pub(crate) overlay: OverlayHandle,
}

pub(crate) fn start(
    endpoint: String,
    tab_id: String,
    recording_id: String,
    recording_directory: PathBuf,
    width: u32,
    height: u32,
    protected_paths: &[PathBuf],
    options: agent_protocol::preview::PreviewRecordingOptions,
    cancel: tokio_util::sync::CancellationToken,
    stop: tokio_util::sync::CancellationToken,
) -> Result<StartResult, String> {
    std::fs::create_dir_all(&recording_directory)
        .map_err(|error| format!("recording storage is unavailable: {error}"))?;
    prune_directory(&recording_directory, protected_paths)?;
    let artifact_path = recording_directory.join(format!("{recording_id}.webm"));
    let started_at = chrono::Utc::now().to_rfc3339();
    let (startup_sender, startup) = oneshot::channel();
    let externally_detached = Arc::new(AtomicBool::new(false));
    let overlay = new_overlay();
    let task = tokio::spawn(run(
        endpoint,
        tab_id,
        recording_directory,
        recording_id.clone(),
        width,
        height,
        options,
        overlay.clone(),
        cancel,
        stop,
        startup_sender,
        externally_detached.clone(),
    ));
    Ok(StartResult {
        recording_id,
        started_at,
        artifact_path,
        externally_detached,
        startup,
        task,
        overlay,
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
    options: agent_protocol::preview::PreviewRecordingOptions,
    overlay: OverlayHandle,
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
        options,
        overlay,
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
                id: id.clone(),
                recording_id: id,
                tab_id,
                path: final_path.to_string_lossy().into_owned(),
                mime_type: MIME_TYPE.into(),
                size_bytes,
                created_at: chrono::Utc::now().to_rfc3339(),
            })
        }
        Err(error) => Err(error),
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
    options: agent_protocol::preview::PreviewRecordingOptions,
    overlay: OverlayHandle,
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
        tab_id,
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
        tab_id,
        Some(&session),
        "Page.enable",
        json!({}),
        &cancel,
        &stop,
    )
    .await
    {
        let error = with_cleanup_error(
            error,
            cleanup_cdp(&mut socket, &mut next_id, tab_id, &session).await,
        );
        note_target_detached(&error, &externally_detached);
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    let mut encoder = match Encoder::start(output, options.frame_rate).await {
        Ok(encoder) => encoder,
        Err(error) => {
            let error = with_cleanup_error(
                error,
                cleanup_cdp(&mut socket, &mut next_id, tab_id, &session).await,
            );
            note_target_detached(&error, &externally_detached);
            notify_startup(&mut startup, Err(error.clone()));
            return Err(error);
        }
    };
    if let Err(error) = command_with_cancel(
        &mut socket,
        &mut next_id,
        tab_id,
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
        let error = with_cleanup_error(
            error,
            cleanup_cdp(&mut socket, &mut next_id, tab_id, &session).await,
        );
        encoder.abort().await;
        note_target_detached(&error, &externally_detached);
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    if cancel.is_cancelled() || stop.is_cancelled() {
        let error = with_cleanup_error(
            "recording start was cancelled".to_owned(),
            cleanup_cdp(&mut socket, &mut next_id, tab_id, &session).await,
        );
        encoder.abort().await;
        note_target_detached(&error, &externally_detached);
        notify_startup(&mut startup, Err(error.clone()));
        return Err(error);
    }
    notify_startup(&mut startup, Ok(()));
    let deadline = tokio::time::sleep(Duration::from_secs(PREVIEW_RECORDING_MAX_DURATION_SECONDS));
    tokio::pin!(deadline);
    let mut frames = 0u64;
    let mut encoded_frames = 0u64;
    let mut encoded_input_bytes = 0u64;
    let max_encoded_frames =
        PREVIEW_RECORDING_MAX_DURATION_SECONDS.saturating_mul(u64::from(options.frame_rate));
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
            message = next_screencast_frame(&mut socket, tab_id, &session) => {
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
                let repeats = frame_repetition_count(
                    first_timestamp,
                    timestamp,
                    encoded_frames,
                    f64::from(options.frame_rate),
                    max_encoded_frames,
                );
                if encoded_frames.saturating_add(repeats) > max_encoded_frames {
                    break Err("recording capture duration exceeds 120000ms".to_owned());
                }
                let added_bytes = (frame.len() as u64).saturating_mul(repeats);
                if encoded_input_bytes.saturating_add(added_bytes) > MAX_ENCODED_INPUT_BYTES {
                    break Err("recording encoder input exceeds its bounded limit".to_owned());
                }
                let frame = if options.show_key_presses || options.show_mouse_presses {
                    decorate_jpeg(&frame, &overlay, options)?
                } else {
                    frame
                };
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
                        tab_id,
                        Some(&session),
                        "Page.screencastFrameAck",
                        json!({"sessionId":session_id}),
                        &cancel,
                        &stop,
                    ).await {
                        if is_target_detached_error(&error) {
                            externally_detached.store(true, Ordering::Release);
                            detached = true;
                            break capture_termination_result(CaptureTermination::Detached);
                        }
                        break Err(error);
                    }
                }
            }
        }
    };
    let cleanup_result = cleanup_cdp(&mut socket, &mut next_id, tab_id, &session).await;
    let cleanup_detached = cleanup_result
        .as_ref()
        .is_err_and(|error| is_target_detached_error(error));
    if cleanup_detached {
        externally_detached.store(true, Ordering::Release);
        detached = true;
    }
    if let Err(error) = capture_result {
        if detached || is_target_detached_error(&error) {
            externally_detached.store(true, Ordering::Release);
            return encoder.finish().await;
        }
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
    target_id: &str,
    session: &str,
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
        if is_detached_event(&value, target_id, Some(session)) {
            return Ok(ScreencastEvent::Detached);
        }
        if value["method"] != "Page.screencastFrame" {
            continue;
        }
        if value["sessionId"].as_str() != Some(session) {
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

fn is_detached_event(value: &Value, target_id: &str, session: Option<&str>) -> bool {
    match value["method"].as_str() {
        Some("Target.detachedFromTarget") => {
            value["params"]["targetId"].as_str() == Some(target_id)
                && session
                    .is_none_or(|session| value["params"]["sessionId"].as_str() == Some(session))
        }
        Some("Target.targetCrashed") => {
            value["params"]["targetId"].as_str() == Some(target_id)
                && session_event_matches(value, session)
        }
        _ => false,
    }
}

/// Target.targetCrashed is normally emitted without a sessionId. If Chrome
/// includes one, it must still belong to this recording's attached session;
/// an unrelated popup event must never terminate this capture.
fn session_event_matches(value: &Value, session: Option<&str>) -> bool {
    value["sessionId"]
        .as_str()
        .is_none_or(|event_session| session == Some(event_session))
}

fn is_target_detached_error(error: &str) -> bool {
    error.contains("target detached")
}

fn note_target_detached(error: &str, externally_detached: &AtomicBool) {
    if is_target_detached_error(error) {
        externally_detached.store(true, Ordering::Release);
    }
}

async fn cleanup_cdp(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    target_id: &str,
    session: &str,
) -> Result<(), String> {
    // Wait for both responses, with a bound per command.  A failed stop must
    // not prevent the detach from being sent, and a failed detach must not be
    // hidden by the stop error.
    let stop = cleanup_command(
        socket,
        next_id,
        target_id,
        Some(session),
        "Page.stopScreencast",
        json!({}),
    )
    .await;
    let detach = cleanup_command(
        socket,
        next_id,
        target_id,
        None,
        "Target.detachFromTarget",
        json!({"sessionId":session}),
    )
    .await;
    combine_cleanup_results(stop, detach)
}

fn with_cleanup_error(error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => format!("{error}; recording cleanup also failed: {cleanup}"),
    }
}

const CDP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

async fn cleanup_command(
    socket: &mut WebSocketStream<ConnectStream>,
    next_id: &mut u64,
    target_id: &str,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<(), String> {
    tokio::time::timeout(
        CDP_CLEANUP_TIMEOUT,
        command(socket, next_id, target_id, session, method, params),
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
    output_fps: f64,
    max_encoded_frames: u64,
) -> u64 {
    let Some(first) = first_timestamp else {
        return 1;
    };
    let Some(timestamp) =
        timestamp.filter(|timestamp| timestamp.is_finite() && *timestamp >= first)
    else {
        return 1;
    };
    let elapsed_frames = (timestamp - first) * output_fps;
    // Keep the value below the cast bound.  A finite CDP timestamp can still
    // be large enough that the float-to-u64 cast saturates, and adding one to
    // that result used to overflow before the duration cap was checked.
    if !elapsed_frames.is_finite() || elapsed_frames >= max_encoded_frames as f64 {
        return max_encoded_frames.saturating_add(1);
    }
    let target = elapsed_frames.round().max(0.0) as u64;
    target
        .saturating_add(1)
        .saturating_sub(encoded_frames)
        .max(1)
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
    let mut total = files
        .iter()
        .map(|(_, metadata)| metadata.len())
        .sum::<u64>();
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
    target_id: &str,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    tokio::time::timeout(
        CDP_COMMAND_TIMEOUT,
        command_inner(socket, next_id, target_id, session, method, params),
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
    target_id: &str,
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
        if is_detached_event(&value, target_id, session) {
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
    target_id: &str,
    session: Option<&str>,
    method: &str,
    params: Value,
    cancel: &tokio_util::sync::CancellationToken,
    stop: &tokio_util::sync::CancellationToken,
) -> Result<Value, String> {
    tokio::select! {
        result = command(socket, next_id, target_id, session, method, params) => result,
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
    _target_id: &str,
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
    async fn start(output: &Path, frame_rate: u8) -> Result<Self, String> {
        let executable = crate::ffmpeg::executable();
        let mut probe = Command::new(&executable);
        probe
            .args(["-hide_banner", "-loglevel", "error", "-encoders"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let encoders = run_bounded_command(probe).await.map_err(|error| {
            format!("recording initialize-media-recorder failed: ffmpeg is unavailable: {error}")
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
            .arg(frame_rate.to_string())
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
            let detail = validation_detail.chars().take(1024).collect::<String>();
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
        assert!(
            id.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );
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
        let max = PREVIEW_RECORDING_MAX_DURATION_SECONDS * 30;
        assert_eq!(frame_repetition_count(None, Some(10.0), 0, 30.0, max), 1);
        assert_eq!(
            frame_repetition_count(Some(10.0), Some(10.1), 1, 30.0, max),
            3
        );
        assert_eq!(
            frame_repetition_count(Some(10.0), Some(10.1), 4, 30.0, max),
            1
        );
        assert_eq!(
            frame_repetition_count(Some(10.0), Some(9.0), 1, 30.0, max),
            1
        );
        assert_eq!(frame_repetition_count(Some(10.0), None, 1, 30.0, max), 1);
        assert_eq!(
            frame_repetition_count(Some(0.0), Some(f64::MAX), 0, 60.0, 120 * 60),
            120 * 60 + 1
        );
    }

    #[test]
    fn recording_options_bound_frame_rate_and_overlay_preferences() {
        use agent_protocol::preview::PreviewRecordingOptions;
        assert!(PreviewRecordingOptions::default().validate().is_ok());
        assert!(
            PreviewRecordingOptions {
                frame_rate: 60,
                show_key_presses: true,
                show_mouse_presses: true
            }
            .validate()
            .is_ok()
        );
        assert!(
            PreviewRecordingOptions {
                frame_rate: 24,
                ..PreviewRecordingOptions::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn cleanup_reports_both_failed_cdp_commands() {
        let error = combine_cleanup_results(Err("stop failed".into()), Err("detach failed".into()))
            .unwrap_err();
        assert!(error.contains("stop failed"));
        assert!(error.contains("detach failed"));
    }

    #[test]
    fn external_target_close_is_a_capture_termination_event() {
        let detached = json!({
            "method":"Target.detachedFromTarget",
            "params":{"targetId":"target","sessionId":"session"}
        });
        let crashed = json!({
            "method":"Target.targetCrashed",
            "params":{"targetId":"target"}
        });
        assert!(is_detached_event(&detached, "target", Some("session")));
        assert!(is_detached_event(&detached, "target", None));
        assert!(!is_detached_event(&detached, "popup", Some("session")));
        assert!(!is_detached_event(&detached, "target", Some("other")));
        assert!(is_detached_event(&crashed, "target", Some("session")));
        assert!(!is_detached_event(&crashed, "popup", Some("session")));
        assert!(!is_detached_event(
            &json!({
                "method":"Target.targetCrashed",
                "sessionId":"popup-session",
                "params":{"targetId":"target"}
            }),
            "target",
            Some("session")
        ));
        assert!(!is_detached_event(
            &json!({"method":"Page.screencastFrame"}),
            "target",
            Some("session")
        ));
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
                FakeCdpPhase::Encoding | FakeCdpPhase::Screencasting | FakeCdpPhase::Stopping
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
                FakeCdpPhase::Encoding | FakeCdpPhase::Screencasting | FakeCdpPhase::Stopping
            ),
        )
    }

    #[test]
    fn fake_cdp_lifecycle_always_detaches_and_aborts_started_encoders() {
        assert_eq!(
            fake_cleanup_actions(FakeCdpPhase::Connected),
            (false, false, false)
        );
        assert_eq!(
            fake_cleanup_actions(FakeCdpPhase::Attached),
            (false, true, false)
        );
        assert_eq!(
            fake_cleanup_actions(FakeCdpPhase::Encoding),
            (true, true, true)
        );
        assert_eq!(
            fake_cleanup_actions(FakeCdpPhase::Screencasting),
            (true, true, true)
        );
        assert_eq!(
            fake_cleanup_actions(FakeCdpPhase::Stopping),
            (true, true, true)
        );
        assert_eq!(
            fake_cleanup_actions(FakeCdpPhase::Finished),
            (false, false, false)
        );
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
