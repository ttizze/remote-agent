//! Owns the persistent BEX profile and conversation-scoped shared pages.
mod cdp;
mod recording;
pub mod mcp;

use agent_protocol::browser::{
    BrowserAction, BrowserFrame, BrowserRequest, BrowserTab, HEIGHT, WIDTH,
};
use agent_protocol::preview::{
    PreviewAppearance, PreviewRenderedViewportSize, PreviewViewportSetting, PreviewZoom,
};
use base64::Engine;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

struct Page {
    tabs: Vec<String>,
    active: String,
    viewports: HashMap<String, (u32, u32)>,
    preview_tabs: HashSet<String>,
    preview_settings: HashMap<
        String,
        (
            agent_protocol::preview::PreviewAppearance,
            agent_protocol::preview::PreviewZoom,
        ),
    >,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: String::new(),
            viewports: HashMap::new(),
            preview_tabs: HashSet::new(),
            preview_settings: HashMap::new(),
        }
    }
}

impl Page {
    fn viewport(&self) -> (u32, u32) {
        self.viewports
            .get(&self.active)
            .copied()
            .unwrap_or((WIDTH, HEIGHT))
    }

    fn viewport_for(&self, tab_id: &str) -> (u32, u32) {
        self.viewports
            .get(tab_id)
            .copied()
            .unwrap_or((WIDTH, HEIGHT))
    }
}

#[derive(Default)]
struct State {
    chrome: Option<cdp::Chrome>,
    pages: HashMap<String, Page>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordingStartupState {
    Pending,
    Started,
    Failed,
}

struct ActiveRecording {
    cancel: CancellationToken,
    abort: tokio::task::AbortHandle,
    artifact_path: PathBuf,
    startup: watch::Sender<RecordingStartupState>,
    done: tokio::sync::watch::Sender<
        Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
    >,
    started_at: String,
    stopping: bool,
}

type RecordingKey = (String, String);
type RecordingArtifact = agent_protocol::preview::PreviewRecordingArtifact;
const MAX_RETAINED_RECORDINGS: usize = 4;

pub struct Browser {
    profile: PathBuf,
    executable: PathBuf,
    state: Arc<Mutex<State>>,
    recordings: Arc<Mutex<HashMap<(String, String), ActiveRecording>>>,
    recording_artifacts: Arc<Mutex<HashMap<RecordingKey, Vec<RecordingArtifact>>>>,
    stop: tokio_util::sync::CancellationToken,
    bridge_directory: tempfile::TempDir,
    preview: OnceLock<Arc<crate::preview::PreviewManager>>,
    preview_ports: OnceLock<Arc<crate::preview::PortScanner>>,
    terminals: OnceLock<Arc<crate::terminals::Terminals>>,
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Browser {
    pub async fn start(profile: PathBuf) -> Result<Arc<Self>, String> {
        let executable = std::env::var_os("BEX_BROWSER_EXECUTABLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if cfg!(target_os = "macos") {
                    PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
                } else {
                    PathBuf::from("chromium")
                }
            });
        let bridge_directory = tempfile::Builder::new()
            .prefix("bex-browser-")
            // macOS Unix sockets have a 104-byte path limit. Nix's TMPDIR may
            // be nested under a long checkout path; keep the socket root short.
            .tempdir_in("/tmp")
            .map_err(|e| e.to_string())?;
        crate::platform::create_state_directory(bridge_directory.path())
            .map_err(|e| e.to_string())?;
        let browser = Arc::new(Self {
            profile,
            executable,
            bridge_directory,
            state: Arc::new(Mutex::new(State::default())),
            recordings: Arc::new(Mutex::new(HashMap::new())),
            recording_artifacts: Arc::new(Mutex::new(HashMap::new())),
            stop: Default::default(),
            preview: OnceLock::new(),
            preview_ports: OnceLock::new(),
            terminals: OnceLock::new(),
        });
        mcp::listen(&browser)?;
        Ok(browser)
    }
    pub fn set_preview_resources(
        &self,
        preview: Arc<crate::preview::PreviewManager>,
        preview_ports: Arc<crate::preview::PortScanner>,
        terminals: Arc<crate::terminals::Terminals>,
    ) -> Result<(), String> {
        self.preview
            .set(preview)
            .map_err(|_| "preview metadata already configured".to_owned())?;
        self.preview_ports
            .set(preview_ports)
            .map_err(|_| "preview scanner already configured".to_owned())?;
        self.terminals
            .set(terminals)
            .map_err(|_| "terminal metadata already configured".to_owned())
    }

    pub async fn start_preview_recording(
        &self,
        thread: &str,
        tab_id: &str,
    ) -> Result<agent_protocol::preview::PreviewRecordingStatus, String> {
        self.start_preview_recording_with_cancel(thread, tab_id, CancellationToken::new())
            .await
    }

    pub(crate) async fn start_preview_recording_with_cancel(
        &self,
        thread: &str,
        tab_id: &str,
        request_cancel: CancellationToken,
    ) -> Result<agent_protocol::preview::PreviewRecordingStatus, String> {
        if request_cancel.is_cancelled() {
            return Err("recording start was cancelled".to_owned());
        }
        let (endpoint, width, height) = {
            let mut state = self.state.lock().await;
            self.ensure(&mut state, thread).await?;
            let page = state
                .pages
                .get(thread)
                .ok_or_else(|| "preview thread was not found".to_owned())?;
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
            let dimensions = page.viewport_for(tab_id);
            let endpoint = state
                .chrome
                .as_ref()
                .ok_or_else(|| "browser is unavailable".to_owned())?
                .endpoint()
                .to_owned();
            (endpoint, dimensions.0, dimensions.1)
        };
        let key = (thread.to_owned(), tab_id.to_owned());
        // Keep the completed-artifact owner locked through the synchronous
        // storage prune and active-slot insertion.  The monitor takes this
        // lock before publishing a completion, so a completion cannot land
        // between the protected-path snapshot and prune.
        let artifacts = self.recording_artifacts.lock().await;
        let previous_artifact_paths = artifacts
            .get(&key)
            .into_iter()
            .flatten()
            .map(|artifact| PathBuf::from(artifact.path.as_str()))
            .collect::<Vec<_>>();
        let mut protected_paths = artifacts
            .values()
            .flatten()
            .map(|artifact| PathBuf::from(artifact.path.as_str()))
            .collect::<Vec<_>>();
        let mut recordings = self.recordings.lock().await;
        protected_paths.extend(
            recordings
                .values()
                .map(|active| active.artifact_path.clone()),
        );
        if recordings.contains_key(&key) {
            return Err(format!(
                "recording is already active for preview tab {tab_id}"
            ));
        }
        let cancel = CancellationToken::new();
        let started = recording::start(
            endpoint,
            tab_id.to_owned(),
            self.bridge_directory.path().join("recordings"),
            width,
            height,
            &protected_paths,
            cancel.clone(),
            self.stop.clone(),
        )?;
        let recording::StartResult {
            started_at,
            artifact_path,
            startup,
            task,
        } = started;
        let artifact_path_for_failure = artifact_path.clone();
        let abort = task.abort_handle();
        let (done, _) = tokio::sync::watch::channel::<
            Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
        >(None);
        let mut done_receiver = done.subscribe();
        let (startup_state, _) = watch::channel(RecordingStartupState::Pending);
        recordings.insert(
            key.clone(),
            ActiveRecording {
                cancel,
                abort,
                artifact_path,
                startup: startup_state.clone(),
                done: done.clone(),
                started_at: started_at.clone(),
                stopping: false,
            },
        );
        drop(recordings);
        drop(artifacts);
        let recordings = self.recordings.clone();
        let recording_artifacts = self.recording_artifacts.clone();
        let browser_state = self.state.clone();
        let preview = self.preview.get().cloned();
        let monitor_key = key.clone();
        let monitor_tab_id = tab_id.to_owned();
        let monitor_thread = thread.to_owned();
        tokio::spawn(async move {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("recording task terminated: {error}")),
            };
            let mut paths_to_remove = Vec::new();
            if let Ok(artifact) = &result {
                let open_recording_keys = {
                    let state = browser_state.lock().await;
                    state
                        .pages
                        .iter()
                        .flat_map(|(thread, page)| {
                            page.preview_tabs
                                .iter()
                                .map(|tab_id| (thread.clone(), tab_id.clone()))
                        })
                        .collect::<HashSet<_>>()
                };
                let mut artifacts = recording_artifacts.lock().await;
                let entries = artifacts.entry(monitor_key.clone()).or_default();
                entries.push(artifact.clone());
                // One completed artifact per tab is enough for a duplicate
                // stop or a client that is still displaying Save/Attach.
                while entries.len() > 1 {
                    paths_to_remove.push(PathBuf::from(entries.remove(0).path));
                }
                while artifacts.len() > MAX_RETAINED_RECORDINGS {
                    let Some(oldest_key) = artifacts
                        .iter()
                        .filter(|(key, _)| !open_recording_keys.contains(*key))
                        .filter_map(|(key, entries)| {
                            entries
                                .first()
                                .map(|artifact| (key.clone(), artifact.created_at.clone()))
                        })
                        .min_by(|(_, left), (_, right)| left.cmp(right))
                        .map(|(key, _)| key)
                    else {
                        break;
                    };
                    if let Some(entries) = artifacts.remove(&oldest_key) {
                        paths_to_remove.extend(
                            entries
                                .into_iter()
                                .map(|artifact| PathBuf::from(artifact.path)),
                        );
                    }
                }
            }
            drop(recording_artifacts);
            for path in paths_to_remove {
                let _ = tokio::fs::remove_file(path).await;
            }
            done.send_replace(Some(result));
            if let Some(preview) = preview
                && let Ok(thread_id) = agent_domain::ThreadId::new(monitor_thread)
            {
                preview.recording_finished(&thread_id, &monitor_tab_id);
            }
            recordings.lock().await.remove(&monitor_key);
        });
        let startup_future = recording::await_startup(startup);
        tokio::pin!(startup_future);
        let (startup_result, request_cancelled) = tokio::select! {
            result = &mut startup_future => (result, request_cancel.is_cancelled()),
            _ = request_cancel.cancelled() => (startup_future.as_mut().await, true),
        };
        startup_state.send_replace(if startup_result.is_ok() {
            RecordingStartupState::Started
        } else {
            RecordingStartupState::Failed
        });
        if request_cancelled {
            self.cancel_recording(&key, &mut done_receiver, Duration::from_secs(5))
                .await;
            self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                .await;
            return Err(format!("recording start was cancelled for tab {tab_id}"));
        }
        if let Err(error) = startup_result {
            self.cancel_recording(&key, &mut done_receiver, Duration::from_secs(5))
                .await;
            self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                .await;
            return Err(format!("recording failed for tab {tab_id}: {error}"));
        }
        if request_cancel.is_cancelled() {
            self.cancel_recording(&key, &mut done_receiver, Duration::from_secs(5))
                .await;
            self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                .await;
            return Err(format!("recording start was cancelled for tab {tab_id}"));
        }
        let status = agent_protocol::preview::PreviewRecordingStatus {
            tab_id: tab_id.to_owned(),
            recording: true,
            started_at: Some(started_at),
        };
        if !self.recordings.lock().await.contains_key(&key) {
            self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                .await;
            return Err(format!("recording stopped during startup for tab {tab_id}"));
        }
        if let Some(preview) = self.preview.get()
            && let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned())
        {
            if let Err(error) = preview.recording_started(thread_id, status.clone()) {
                self.cancel_recording(&key, &mut done_receiver, Duration::from_secs(5))
                    .await;
                self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                    .await;
                return Err(error);
            }
            if !self.recordings.lock().await.contains_key(&key) {
                if let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned()) {
                    preview.recording_finished(&thread_id, tab_id);
                }
                self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                    .await;
                return Err(format!("recording stopped during startup for tab {tab_id}"));
            }
        }
        // A successful start clears the previous core-offered artifact.  Keep
        // it protected until this point so a failed start never leaves Save or
        // Attach pointing at a pruned file.
        self.clear_replaced_recording_artifacts(&key, &previous_artifact_paths)
            .await;
        Ok(status)
    }

    pub async fn stop_preview_recording(
        &self,
        thread: &str,
        tab_id: &str,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        self.stop_preview_recording_with_timeout(
            thread,
            tab_id,
            Duration::from_secs(
                agent_protocol::preview::PREVIEW_RECORDING_MAX_DURATION_SECONDS,
            ),
            None,
        )
        .await
    }

    pub(crate) async fn stop_preview_recording_with_cancel(
        &self,
        thread: &str,
        tab_id: &str,
        request_cancel: CancellationToken,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        self.stop_preview_recording_with_timeout(
            thread,
            tab_id,
            Duration::from_secs(
                agent_protocol::preview::PREVIEW_RECORDING_MAX_DURATION_SECONDS,
            ),
            Some(request_cancel),
        )
        .await
    }

    async fn stop_preview_recording_with_timeout(
        &self,
        thread: &str,
        tab_id: &str,
        timeout: Duration,
        request_cancel: Option<CancellationToken>,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        let key = (thread.to_owned(), tab_id.to_owned());
        let active = {
            let mut recordings = self.recordings.lock().await;
            match recordings.get_mut(&key) {
                Some(active) => {
                    let initiate_stop = begin_recording_stop(&mut active.stopping);
                    Some((
                        active.cancel.clone(),
                        active.abort.clone(),
                        active.done.subscribe(),
                        active.startup.subscribe(),
                        initiate_stop,
                    ))
                }
                None => None,
            }
        };
        let Some((cancel, abort, mut done, mut startup, initiate_stop)) = active else {
            return self
                .completed_recording(&key)
                .await
                .ok_or_else(|| format!("recording is not active for preview tab {tab_id}"));
        };
        if initiate_stop {
            let startup_result = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_recording_startup(&mut startup),
            )
            .await;
            if let Err(_) = startup_result {
                abort.abort();
                return Err(format!(
                    "recording startup did not settle before stopping for preview tab {tab_id}"
                ));
            }
            cancel.cancel();
        }
        let result = if let Some(request_cancel) = request_cancel {
            tokio::select! {
                result = tokio::time::timeout(timeout, wait_for_recording_completion(&mut done)) => result.map_err(|_| {
                    format!(
                        "recording stop timeout for tab {tab_id} after {}ms",
                        timeout.as_millis()
                    )
                }),
                _ = request_cancel.cancelled() => {
                    return Err(format!("recording stop was cancelled for tab {tab_id}"));
                }
            }
        } else {
            tokio::time::timeout(timeout, wait_for_recording_completion(&mut done))
                .await
                .map_err(|_| {
                    format!(
                        "recording stop timeout for tab {tab_id} after {}ms",
                        timeout.as_millis()
                    )
                })
        };
        let result = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) | Err(error) => {
                if initiate_stop {
                    abort.abort();
                }
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    wait_for_recording_completion(&mut done),
                )
                .await;
                return Err(error);
            }
        }?;
        result
    }

    async fn completed_recording(&self, key: &RecordingKey) -> Option<RecordingArtifact> {
        self.recording_artifacts
            .lock()
            .await
            .get(key)
            .and_then(|artifacts| artifacts.last().cloned())
    }

    async fn clear_recording_artifacts(&self, key: &RecordingKey) {
        let paths = self
            .recording_artifacts
            .lock()
            .await
            .remove(key)
            .unwrap_or_default()
            .into_iter()
            .map(|artifact| PathBuf::from(artifact.path))
            .collect::<Vec<_>>();
        for path in paths {
            let _ = tokio::fs::remove_file(path).await;
        }
    }

    async fn discard_recording_artifact_path(&self, key: &RecordingKey, path: &PathBuf) {
        let target = path.to_string_lossy().into_owned();
        let is_offered = self
            .recording_artifacts
            .lock()
            .await
            .get(key)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|artifact| artifact.path.as_str() == target.as_str())
            });
        if !is_offered {
            let _ = tokio::fs::remove_file(path).await;
        }
    }

    async fn clear_replaced_recording_artifacts(
        &self,
        key: &RecordingKey,
        previous_paths: &[PathBuf],
    ) {
        if previous_paths.is_empty() {
            return;
        }
        let removed = {
            let mut artifacts = self.recording_artifacts.lock().await;
            let entries = artifacts.remove(key).unwrap_or_default();
            let mut removed = Vec::new();
            let mut retained = Vec::new();
            for artifact in entries {
                let path = PathBuf::from(artifact.path.as_str());
                if previous_paths.contains(&path) {
                    removed.push(path);
                } else {
                    retained.push(artifact);
                }
            }
            if !retained.is_empty() {
                artifacts.insert(key.clone(), retained);
            }
            removed
        };
        for path in removed {
            let _ = tokio::fs::remove_file(path).await;
        }
    }

    async fn cancel_recording(
        &self,
        key: &(String, String),
        done: &mut tokio::sync::watch::Receiver<
            Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
        >,
        timeout: Duration,
    ) {
        let (cancel, abort) = {
            let mut recordings = self.recordings.lock().await;
            let Some(active) = recordings.get_mut(key) else { return; };
            active.stopping = true;
            (active.cancel.clone(), active.abort.clone())
        };
        cancel.cancel();
        match tokio::time::timeout(timeout, wait_for_recording_completion(done)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => {
                abort.abort();
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    wait_for_recording_completion(done),
                )
                .await;
            }
        }
    }

    pub async fn active_recording_tab(&self, thread: &str) -> Result<String, String> {
        let recordings = self.recordings.lock().await;
        let mut tabs = recordings
            .keys()
            .filter(|(scope, _)| scope == thread)
            .map(|(_, tab)| tab.clone());
        match (tabs.next(), tabs.next()) {
            (Some(tab), None) => Ok(tab),
            (None, _) => Err("no Preview recording is active for this conversation".into()),
            (Some(_), Some(_)) => Err("multiple Preview recordings are active; specify tab_id".into()),
        }
    }

    pub async fn preview_active_tab(&self, thread: &str) -> Result<String, String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let page = state
            .pages
            .get(thread)
            .ok_or_else(|| "preview thread was not found".to_owned())?;
        if page.preview_tabs.contains(&page.active) {
            Ok(page.active.clone())
        } else {
            Err("preview tab is not open".to_owned())
        }
    }

    pub fn provider_config(&self, thread: &str) -> Result<serde_json::Value, String> {
        Ok(
            serde_json::json!({"command":std::env::current_exe().map_err(|e| e.to_string())?,
            "args":["browser-mcp", "--socket", self.socket(), "--thread", thread]}),
        )
    }
    pub(crate) fn profile(&self) -> &std::path::Path {
        &self.profile
    }
    fn socket(&self) -> PathBuf {
        self.bridge_directory.path().join("bridge.sock")
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let keys = self.recordings.lock().await.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            let (mut done, mut startup) = {
                let mut recordings = self.recordings.lock().await;
                let Some(active) = recordings.get_mut(&key) else { continue; };
                active.stopping = true;
                (active.done.subscribe(), active.startup.subscribe())
            };
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_recording_startup(&mut startup),
            )
            .await;
            if let Some(active) = self.recordings.lock().await.get(&key) {
                active.cancel.cancel();
            }
            match tokio::time::timeout(
                Duration::from_secs(120),
                wait_for_recording_completion(&mut done),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => {
                    if let Some(active) = self.recordings.lock().await.get(&key) {
                        active.abort.abort();
                    }
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        wait_for_recording_completion(&mut done),
                    )
                    .await;
                }
            }
        }
        if let Some(chrome) = self.state.lock().await.chrome.take() {
            chrome.shutdown().await;
        }
    }

    async fn ensure(&self, state: &mut State, thread: &str) -> Result<(), String> {
        if thread.is_empty() || thread.len() > 8192 {
            return Err("browser scope is required".into());
        }
        if state.chrome.as_ref().is_some_and(|chrome| chrome.broken) {
            if let Some(chrome) = state.chrome.take() {
                chrome.shutdown().await;
            }
            for page in state.pages.values_mut() {
                page.tabs.clear();
                page.active.clear();
                page.viewports.clear();
                page.preview_tabs.clear();
                page.preview_settings.clear();
            }
        }
        if state.chrome.is_none() {
            state.chrome = Some(cdp::Chrome::launch(&self.profile, &self.executable).await?);
        }
        if !state.pages.contains_key(thread) && state.pages.len() >= 32 {
            return Err("ブラウザを開いている会話が上限に達しました。".into());
        }
        let page = state.pages.entry(thread.into()).or_default();
        let chrome = state.chrome.as_mut().unwrap();
        let targets = chrome.targets().await?;
        // Include transitive popups only from this conversation's known targets.
        loop {
            let added: Vec<_> = targets
                .iter()
                .filter(|target| {
                    target.kind == "page"
                        && !page.tabs.contains(&target.target_id)
                        && target
                            .opener_id
                            .as_ref()
                            .is_some_and(|id| page.tabs.contains(id))
                })
                .map(|target| target.target_id.clone())
                .collect();
            if added.is_empty() {
                break;
            }
            page.active = added.last().unwrap().clone();
            page.tabs.extend(added);
        }
        page.tabs
            .retain(|id| targets.iter().any(|target| &target.target_id == id));
        page.viewports
            .retain(|id, _| targets.iter().any(|target| &target.target_id == id));
        page.preview_tabs
            .retain(|id| targets.iter().any(|target| &target.target_id == id));
        page.preview_settings
            .retain(|id, _| targets.iter().any(|target| &target.target_id == id));
        if page.tabs.is_empty() {
            let id = chrome.create().await?;
            page.tabs.push(id.clone());
            page.active = id;
            page.viewports.insert(page.active.clone(), (WIDTH, HEIGHT));
        } else if !page.tabs.contains(&page.active) {
            page.active = page.tabs.last().unwrap().clone();
        }
        Ok(())
    }

    pub async fn request(&self, request: &BrowserRequest) -> Result<BrowserFrame, String> {
        let thread = request.thread_id.to_string();
        let mut state = self.state.lock().await;
        self.ensure(&mut state, &thread).await?;
        let viewport = state.pages[&thread].viewport();
        request.validate_for_viewport(viewport.0, viewport.1)?;
        validate_tab(
            &state.pages[&thread].active,
            &request.tab_id,
            &request.action,
        )?;
        Self::action(&mut state, &thread, &request.action).await?;
        let mut frame = Self::frame(&mut state, &thread).await?;
        let changed = frame.image_id != request.image_id;
        if !changed {
            frame.image.clear();
        }
        drop(state);
        if changed {
            self.persist_artifact(&thread, &frame).await;
        }
        self.report_preview_frame(&thread, &frame);
        Ok(frame)
    }

    pub(super) async fn agent(
        &self,
        thread: &str,
        action: BrowserAction,
    ) -> Result<BrowserFrame, String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let viewport = state.pages[thread].viewport();
        action.validate_for_viewport(viewport.0, viewport.1)?;
        Self::action(&mut state, thread, &action).await?;
        let frame = Self::frame(&mut state, thread).await?;
        drop(state);
        self.persist_artifact(thread, &frame).await;
        self.report_preview_frame(thread, &frame);
        Ok(frame)
    }

    /// Creates a Host browser tab for the Preview surface and returns its
    /// first frame. Existing agent browser tabs remain in the same conversation
    /// scope and are never replaced.
    pub async fn open_preview_tab(
        &self,
        thread: &str,
        url: Option<&str>,
        viewport: PreviewViewportSetting,
        appearance: PreviewAppearance,
        zoom: PreviewZoom,
        rendered_size: Option<PreviewRenderedViewportSize>,
    ) -> Result<BrowserFrame, String> {
        viewport.validate()?;
        if let Some(size) = rendered_size {
            size.validate()?;
        }
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let resource_viewport = state.pages[thread].viewport();
        let (width, height) = viewport
            .dimensions()
            .or_else(|| rendered_size.map(|size| (size.width, size.height)))
            .unwrap_or(resource_viewport);
        let id = {
            let chrome = state.chrome.as_mut().unwrap();
            chrome.create().await?
        };
        let page = state.pages.get_mut(thread).unwrap();
        page.tabs.push(id.clone());
        page.active = id.clone();
        page.viewports.insert(id.clone(), (width, height));
        page.preview_tabs.insert(page.active.clone());
        page.preview_settings.insert(
            page.active.clone(),
            (appearance, zoom),
        );
        let session = {
            let chrome = state.chrome.as_mut().unwrap();
            chrome.attach(&id, width, height).await?
        };
        {
            let chrome = state.chrome.as_mut().unwrap();
            chrome.set_appearance(&session, appearance).await?;
            chrome.set_zoom(&session, zoom).await?;
        }
        if let Some(url) = url {
            let action = BrowserAction::Navigate { url: url.to_owned() };
            Self::action(&mut state, thread, &action).await?;
        }
        Self::frame(&mut state, thread).await
    }

    async fn persist_artifact(&self, thread: &str, frame: &BrowserFrame) {
        if frame.image.is_empty() {
            return;
        }
        let component = |value: &str| {
            value
                .chars()
                .map(|character| {
                    character
                        .is_ascii_alphanumeric()
                        .then_some(character)
                        .or_else(|| matches!(character, '-' | '_').then_some(character))
                        .unwrap_or('_')
                })
                .collect::<String>()
        };
        let directory = self.profile.join("artifacts");
        let path = directory.join(format!(
            "{}-{}.png",
            component(thread),
            component(&frame.image_id)
        ));
        if let Err(error) = tokio::fs::create_dir_all(&directory).await {
            tracing::debug!(%error, "could not create browser artifact directory");
            return;
        }
        if let Err(error) = tokio::fs::write(path, &frame.image).await {
            tracing::debug!(%error, "could not persist browser screenshot artifact");
        }
    }

    pub async fn resize_preview_tab(
        &self,
        thread: &str,
        tab_id: &str,
        viewport: PreviewViewportSetting,
        rendered_size: Option<PreviewRenderedViewportSize>,
    ) -> Result<BrowserFrame, String> {
        viewport.validate()?;
        if let Some(size) = rendered_size {
            size.validate()?;
        }
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let dimensions = viewport
            .dimensions()
            .or_else(|| rendered_size.map(|size| (size.width, size.height)))
            .unwrap_or_else(|| state.pages[thread].viewport_for(tab_id));
        {
            let page = state.pages.get_mut(thread).unwrap();
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
            page.active = tab_id.to_owned();
            page.viewports.insert(tab_id.to_owned(), dimensions);
        }
        Self::frame(&mut state, thread).await
    }

    pub async fn set_preview_appearance(
        &self,
        thread: &str,
        tab_id: &str,
        appearance: agent_protocol::preview::PreviewAppearance,
    ) -> Result<BrowserFrame, String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let viewport = {
            let page = state.pages.get_mut(thread).unwrap();
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
            page.active = tab_id.to_owned();
            page.viewport_for(tab_id)
        };
        let active = tab_id.to_owned();
        {
            let chrome = state.chrome.as_mut().unwrap();
            let session = chrome.attach(&active, viewport.0, viewport.1).await?;
            chrome.set_appearance(&session, appearance).await?;
        }
        state.pages
            .get_mut(thread)
            .unwrap()
            .preview_settings
            .entry(active.clone())
            .and_modify(|settings| settings.0 = appearance);
        Self::frame(&mut state, thread).await
    }

    pub async fn set_preview_zoom(
        &self,
        thread: &str,
        tab_id: &str,
        zoom: agent_protocol::preview::PreviewZoom,
    ) -> Result<BrowserFrame, String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let viewport = {
            let page = state.pages.get_mut(thread).unwrap();
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
            page.active = tab_id.to_owned();
            page.viewport_for(tab_id)
        };
        let active = tab_id.to_owned();
        {
            let chrome = state.chrome.as_mut().unwrap();
            let session = chrome.attach(&active, viewport.0, viewport.1).await?;
            chrome.set_zoom(&session, zoom).await?;
        }
        state.pages
            .get_mut(thread)
            .unwrap()
            .preview_settings
            .entry(active.clone())
            .and_modify(|settings| settings.1 = zoom);
        Self::frame(&mut state, thread).await
    }

    pub async fn close_preview_tab(&self, thread: &str, tab_id: &str) -> Result<(), String> {
        self.close_preview_tab_with_cancel(thread, tab_id, CancellationToken::new())
            .await
    }

    pub(crate) async fn close_preview_tab_with_cancel(
        &self,
        thread: &str,
        tab_id: &str,
        request_cancel: CancellationToken,
    ) -> Result<(), String> {
        if request_cancel.is_cancelled() {
            return Err(format!("closing Preview tab {tab_id} was cancelled"));
        }
        let key = (thread.to_owned(), tab_id.to_owned());
        let stop_result = if self.recordings.lock().await.contains_key(&key) {
            let result = self
                .stop_preview_recording_with_timeout(
                    thread,
                    tab_id,
                    Duration::from_secs(
                        agent_protocol::preview::PREVIEW_RECORDING_MAX_DURATION_SECONDS,
                    ),
                    Some(request_cancel.clone()),
                )
                .await;
            match result {
                Err(error) if error.starts_with("recording is not active") => None,
                result => Some(result),
            }
        } else {
            None
        };
        if request_cancel.is_cancelled() {
            return Err(format!("closing Preview tab {tab_id} was cancelled"));
        }
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        {
            let page = state.pages.get(thread).unwrap();
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
        }
        let chrome = state.chrome.as_mut().unwrap();
        let close_result = tokio::select! {
            result = chrome.close_target(tab_id) => result,
            _ = request_cancel.cancelled() => {
                return Err(format!("closing Preview tab {tab_id} was cancelled"));
            }
        };
        if let Err(error) = close_result {
            return Err(match stop_result {
                Some(Err(stop_error)) => {
                    format!("closing Preview tab failed: {error}; recording stop failed: {stop_error}")
                }
                _ => error,
            });
        }
        let page = state.pages.get_mut(thread).unwrap();
        page.tabs.retain(|id| id != tab_id);
        page.viewports.remove(tab_id);
        page.preview_tabs.remove(tab_id);
        page.preview_settings.remove(tab_id);
        if page.active == tab_id {
            page.active = page.tabs.last().cloned().unwrap_or_default();
        }
        drop(state);
        if stop_result.is_none() || stop_result.as_ref().is_some_and(|result| result.is_ok()) {
            self.clear_recording_artifacts(&key).await;
        }
        if let Some(preview) = self.preview.get()
            && let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned())
        {
            preview.close(&thread_id, Some(tab_id));
        }
        if let Some(Err(error)) = stop_result {
            return Err(format!(
                "Preview tab closed but recording stop failed: {error}"
            ));
        }
        Ok(())
    }

    /// Returns the tabs currently owned by the shared browser page.  The RPC
    /// Preview manager remains the authoritative metadata owner; this view is
    /// used only by the provider bridge's preview discovery tool.
    pub async fn preview_list(
        &self,
        thread: &str,
    ) -> Result<agent_protocol::preview::PreviewListResult, String> {
        let thread_id = agent_domain::ThreadId::new(thread.to_owned())
            .map_err(|_| "browser scope is invalid".to_owned())?;
        let mut browser_result = {
            let mut state = self.state.lock().await;
            self.ensure(&mut state, thread).await?;
            let (tabs, viewports, settings) = {
                let page = state.pages.get(thread).unwrap();
                (
                    page.preview_tabs.clone(),
                    page.viewports.clone(),
                    page.preview_settings.clone(),
                )
            };
            let chrome = state.chrome.as_mut().unwrap();
            let targets = chrome.targets().await?;
            let sessions = targets
                .into_iter()
                .filter(|target| tabs.contains(&target.target_id))
                .map(|target| {
                    let (width, height) = viewports
                        .get(&target.target_id)
                        .copied()
                        .unwrap_or((WIDTH, HEIGHT));
                    let (appearance, zoom) = settings
                        .get(&target.target_id)
                        .copied()
                        .unwrap_or((
                            agent_protocol::preview::PreviewAppearance::System,
                            agent_protocol::preview::PreviewZoom::X100,
                        ));
                    agent_protocol::preview::PreviewSessionSnapshot {
                        thread_id: thread_id.clone(),
                        tab_id: target.target_id,
                        nav_status: if target.url.is_empty() || target.url == "about:blank" {
                            agent_protocol::preview::PreviewNavStatus::Idle
                        } else {
                            agent_protocol::preview::PreviewNavStatus::Success {
                                url: target.url,
                                title: target.title,
                            }
                        },
                        can_go_back: false,
                        can_go_forward: false,
                        viewport: agent_protocol::preview::PreviewViewportSetting::Freeform {
                            width,
                            height,
                        },
                        zoom,
                        appearance,
                        updated_at: String::new(),
                    }
                })
                .collect();
            agent_protocol::preview::PreviewListResult {
                sessions,
                recordings: Vec::new(),
                local_servers: Vec::new(),
                scanned_at: String::new(),
                server_epoch: String::new(),
                revision: 0,
                scanner_epoch: String::new(),
                scanner_revision: 0,
            }
        };
        browser_result.recordings = self
            .recordings
            .lock()
            .await
            .iter()
            .filter(|((scope, _), _)| scope == thread)
            .map(|((_, tab_id), active)| agent_protocol::preview::PreviewRecordingStatus {
                tab_id: tab_id.clone(),
                recording: true,
                started_at: Some(active.started_at.clone()),
            })
            .collect();
        let (Some(preview), Some(preview_ports), Some(terminals)) = (
            self.preview.get(),
            self.preview_ports.get(),
            self.terminals.get(),
        ) else {
            return Ok(browser_result);
        };
        preview_ports.set_terminal_owners(terminals.preview_process_owners());
        let discovered = preview_ports
            .scan_snapshot(&[], &terminals.summaries_now())
            .await?;
        let mut result = preview.list(&thread_id);
        if result.sessions.is_empty() {
            result.sessions = browser_result.sessions;
        }
        result.local_servers = discovered.servers;
        result.scanned_at = discovered.scanned_at;
        result.scanner_epoch = discovered.epoch;
        result.scanner_revision = discovered.revision;
        Ok(result)
    }

    async fn action(state: &mut State, thread: &str, action: &BrowserAction) -> Result<(), String> {
        if let BrowserAction::SelectTab { id } = action {
            let page = state.pages.get_mut(thread).unwrap();
            if !page.tabs.contains(id) {
                return Err("この会話のタブではありません。".into());
            }
            page.active = id.clone();
            return Ok(());
        }
        if matches!(action, BrowserAction::Read) {
            return Ok(());
        }
        let (active, viewport) = {
            let page = state.pages.get(thread).unwrap();
            (page.active.clone(), page.viewport())
        };
        let chrome = state.chrome.as_mut().unwrap();
        let session = chrome
            .attach(&active, viewport.0, viewport.1)
            .await?;
        chrome.action(&session, action).await
    }

    pub(super) fn report_preview_frame(&self, thread: &str, frame: &BrowserFrame) {
        let (Some(preview), Ok(thread_id)) = (
            self.preview.get(),
            agent_domain::ThreadId::new(thread.to_owned()),
        ) else {
            return;
        };
        let Ok(previous) = preview.get(&thread_id, &frame.tab_id) else {
            return;
        };
        let Some(tab) = frame.tabs.iter().find(|tab| tab.id == frame.tab_id) else {
            return;
        };
        let nav_status = if tab.url.is_empty() || tab.url == "about:blank" {
            agent_protocol::preview::PreviewNavStatus::Idle
        } else if let Ok(url) = agent_protocol::preview::normalize_preview_url(&tab.url) {
            agent_protocol::preview::PreviewNavStatus::Success {
                url,
                title: tab
                    .title
                    .chars()
                    .take(agent_protocol::preview::PREVIEW_TITLE_MAX_LENGTH)
                    .collect(),
            }
        } else {
            previous.nav_status.clone()
        };
        let _ = preview.report_status(
            &thread_id,
            &frame.tab_id,
            nav_status,
            previous.can_go_back,
            previous.can_go_forward,
        );
    }

    async fn frame(state: &mut State, thread: &str) -> Result<BrowserFrame, String> {
        let (active, viewport, tabs) = {
            let page = &state.pages[thread];
            (page.active.clone(), page.viewport(), page.tabs.clone())
        };
        let chrome = state.chrome.as_mut().unwrap();
        let session = chrome
            .attach(&active, viewport.0, viewport.1)
            .await?;
        let image = chrome.screenshot(&session).await?;
        let image_id = base64::engine::general_purpose::STANDARD
            .encode(ring::digest::digest(&ring::digest::SHA256, &image));
        let targets = chrome.targets().await?;
        Ok(BrowserFrame {
            tabs: targets
                .into_iter()
                .filter(|target| tabs.contains(&target.target_id))
                .map(|target| BrowserTab {
                    id: target.target_id,
                    title: target.title,
                    url: target.url,
                })
                .collect(),
            tab_id: active,
            width: viewport.0,
            height: viewport.1,
            image,
            image_id,
            dialog: chrome.dialog(&session),
        })
    }
}

fn begin_recording_stop(stopping: &mut bool) -> bool {
    let initiate_stop = !*stopping;
    *stopping = true;
    initiate_stop
}

async fn wait_for_recording_completion(
    done: &mut tokio::sync::watch::Receiver<
        Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
    >,
) -> Result<Result<agent_protocol::preview::PreviewRecordingArtifact, String>, String> {
    loop {
        let completed = done.borrow().clone();
        if let Some(result) = completed {
            return Ok(result);
        }
        done.changed()
            .await
            .map_err(|_| "recording completion channel closed".to_owned())?;
    }
}

async fn wait_for_recording_startup(
    startup: &mut watch::Receiver<RecordingStartupState>,
) -> Result<RecordingStartupState, String> {
    loop {
        let state = *startup.borrow();
        if state != RecordingStartupState::Pending {
            return Ok(state);
        }
        startup
            .changed()
            .await
            .map_err(|_| "recording startup channel closed".to_owned())?;
    }
}

fn validate_tab(active: &str, displayed: &str, action: &BrowserAction) -> Result<(), String> {
    if !matches!(
        action,
        BrowserAction::Read | BrowserAction::SelectTab { .. }
    ) && active != displayed
    {
        return Err("表示中のタブが変わりました。最新の画面で再試行してください。".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
