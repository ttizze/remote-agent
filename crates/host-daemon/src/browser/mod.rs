//! Owns the persistent BEX profile and conversation-scoped shared pages.
mod cdp;
pub mod mcp;
mod recording;

use agent_protocol::browser::{
    BrowserAction, BrowserFrame, BrowserRequest, BrowserTab, HEIGHT, WIDTH,
};
use agent_protocol::preview::{
    PreviewAppearance, PreviewRenderedViewportSize, PreviewViewportSetting, PreviewZoom,
};
use base64::Engine;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Page {
    tabs: Vec<String>,
    active: String,
    /// Each authenticated client keeps its own selection.  The `active`
    /// field remains the default browser selection for unscoped internal
    /// reconciliation; a Preview or MCP request never changes another
    /// client's selection.
    active_by_owner: HashMap<String, String>,
    viewports: HashMap<String, (u32, u32)>,
    preview_tabs: HashSet<String>,
    preview_profiles: HashMap<String, Option<String>>,
    /// The authenticated Host principal that owns each Preview tab.  The
    /// tab id is global to the Chromium target set, while the profile
    /// process is partitioned by this owner and profile id.
    preview_profile_owners: HashMap<String, String>,
    preview_settings: HashMap<
        String,
        (
            agent_protocol::preview::PreviewAppearance,
            agent_protocol::preview::PreviewZoom,
        ),
    >,
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

    fn selected_for_owner(&self, owner: Option<&str>) -> Option<&str> {
        owner
            .and_then(|owner| self.active_by_owner.get(owner).map(String::as_str))
            .or_else(|| (!self.active.is_empty()).then_some(self.active.as_str()))
    }

    fn set_selected_for_owner(&mut self, owner: Option<&str>, tab_id: String) {
        if let Some(owner) = owner {
            self.active_by_owner.insert(owner.to_owned(), tab_id);
        } else {
            self.active = tab_id;
        }
    }

    fn remove_from_selections(&mut self, tab_id: &str) {
        self.active_by_owner
            .retain(|_, selected| selected != tab_id);
        if self.active == tab_id {
            self.active = self.tabs.last().cloned().unwrap_or_default();
        }
    }

    fn retain_valid_selections(&mut self) {
        let tabs = self.tabs.iter().cloned().collect::<HashSet<_>>();
        self.active_by_owner
            .retain(|_, selected| tabs.contains(selected));
    }
}

pub(crate) const COLLABORATIVE_BROWSER_OWNER: &str = "local";

fn can_access_preview_tab(page: &Page, owner: Option<&str>, tab_id: &str) -> bool {
    !page.preview_tabs.contains(tab_id)
        || owner.is_none_or(|owner| {
            owner == COLLABORATIVE_BROWSER_OWNER
                || page.preview_profile_owners.get(tab_id).map(String::as_str) == Some(owner)
        })
}

#[derive(Default)]
struct State {
    chrome: Option<cdp::Chrome>,
    /// Dedicated Chromium processes for non-default Preview profiles. A CDP
    /// BrowserContext is process-scoped and therefore cannot provide the
    /// persistent partition semantics used by the client settings contract.
    preview_chromes: HashMap<PreviewChromeKey, PreviewChrome>,
    pages: HashMap<String, Page>,
}

struct PreviewChrome {
    chrome: cdp::Chrome,
    profile_path: PathBuf,
    ephemeral: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PreviewChromeKey {
    owner: String,
    profile_id: String,
}

/// The immutable scope attached to one provider MCP token.  The command line
/// thread is retained for diagnostics, but bridge requests always use this
/// server-owned scope rather than trusting a mutable client supplied thread.
#[derive(Clone, Debug)]
pub(crate) struct BrowserBridgeScope {
    pub(crate) owner: String,
    pub(crate) thread: String,
}

impl PreviewChrome {
    async fn shutdown(self) {
        self.chrome.shutdown().await;
        if self.ephemeral {
            let _ = tokio::fs::remove_dir_all(self.profile_path).await;
        }
    }
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
    recording_id: String,
    artifact_path: PathBuf,
    startup: watch::Sender<RecordingStartupState>,
    done: tokio::sync::watch::Sender<
        Option<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
    >,
    started_at: String,
    stopping: bool,
    overlay: recording::OverlayHandle,
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
    bridge_endpoint: OnceLock<String>,
    bridge_scopes: std::sync::Mutex<HashMap<String, BrowserBridgeScope>>,
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
    pub(crate) fn profile(&self) -> &Path {
        &self.profile
    }

    pub(crate) fn has_active_tasks(&self) -> bool {
        self.recordings
            .try_lock()
            .map(|recordings| !recordings.is_empty())
            .unwrap_or(true)
    }

    fn preview_profile_key(profile_id: Option<&str>) -> &str {
        profile_id.unwrap_or(agent_protocol::preview::DEFAULT_PREVIEW_PROFILE_ID)
    }

    fn preview_profile_path(&self, owner: &str, profile_id: &str) -> (PathBuf, bool) {
        let suffix = Self::preview_profile_suffix(owner, profile_id);
        if profile_id == agent_protocol::preview::INCOGNITO_PREVIEW_PROFILE_ID {
            (
                self.profile
                    .join("preview-profiles")
                    .join(format!("incognito-{suffix}")),
                true,
            )
        } else {
            (
                self.profile
                    .join("preview-profiles")
                    .join(format!("persistent-{suffix}")),
                false,
            )
        }
    }

    fn preview_profile_suffix(owner: &str, profile_id: &str) -> String {
        let mut identity = Vec::with_capacity(owner.len() + profile_id.len() + 1);
        identity.extend_from_slice(owner.as_bytes());
        identity.push(0);
        identity.extend_from_slice(profile_id.as_bytes());
        let digest = ring::digest::digest(&ring::digest::SHA256, &identity);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref())
    }

    async fn ensure_preview_chrome<'a>(
        &'a self,
        state: &'a mut State,
        owner: &str,
        profile_id: &str,
    ) -> Result<&'a mut PreviewChrome, String> {
        agent_protocol::preview::validate_profile_id(profile_id)?;
        let key = PreviewChromeKey {
            owner: owner.to_owned(),
            profile_id: profile_id.to_owned(),
        };
        if !state.preview_chromes.contains_key(&key) {
            let (profile_path, ephemeral) = self.preview_profile_path(owner, profile_id);
            let chrome = cdp::Chrome::launch(&profile_path, &self.executable).await?;
            state.preview_chromes.insert(
                key.clone(),
                PreviewChrome {
                    chrome,
                    profile_path,
                    ephemeral,
                },
            );
        }
        Ok(state
            .preview_chromes
            .get_mut(&key)
            .expect("preview Chrome inserted above"))
    }

    fn tab_profile(page: &Page, tab_id: &str) -> Option<PreviewChromeKey> {
        let owner = page.preview_profile_owners.get(tab_id)?;
        let profile_id = page
            .preview_profiles
            .get(tab_id)
            .map(|profile| Self::preview_profile_key(profile.as_deref()).to_owned())?;
        Some(PreviewChromeKey {
            owner: owner.clone(),
            profile_id,
        })
    }

    fn chrome_for_profile<'a>(
        state: &'a mut State,
        owner: Option<&str>,
        profile_id: Option<&str>,
    ) -> Result<&'a mut cdp::Chrome, String> {
        match owner {
            None => state
                .chrome
                .as_mut()
                .ok_or_else(|| "browser is unavailable".to_owned()),
            Some(owner) => state
                .preview_chromes
                .get_mut(&PreviewChromeKey {
                    owner: owner.to_owned(),
                    profile_id: Self::preview_profile_key(profile_id).to_owned(),
                })
                .map(|preview| &mut preview.chrome)
                .ok_or_else(|| {
                    format!(
                        "browser profile {} is unavailable",
                        Self::preview_profile_key(profile_id)
                    )
                }),
        }
    }

    fn chrome_for_key(
        state: &mut State,
        key: Option<PreviewChromeKey>,
    ) -> Result<&mut cdp::Chrome, String> {
        match key {
            Some(key) => state
                .preview_chromes
                .get_mut(&key)
                .map(|preview| &mut preview.chrome)
                .ok_or_else(|| format!("browser profile {} is unavailable", key.profile_id)),
            None => state
                .chrome
                .as_mut()
                .ok_or_else(|| "browser is unavailable".to_owned()),
        }
    }

    async fn all_targets(state: &mut State) -> Result<Vec<cdp::Target>, String> {
        let mut targets = if let Some(chrome) = state.chrome.as_mut() {
            chrome.targets().await?
        } else {
            Vec::new()
        };
        for preview in state.preview_chromes.values_mut() {
            targets.extend(preview.chrome.targets().await?);
        }
        Ok(targets)
    }

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
        let bridge_directory = {
            let mut builder = tempfile::Builder::new();
            builder.prefix("bex-browser-");
            #[cfg(unix)]
            {
                // macOS Unix sockets have a 104-byte path limit. Nix's TMPDIR
                // may be nested under a long checkout path; keep the socket
                // root short.
                builder.tempdir_in("/tmp")
            }
            #[cfg(not(unix))]
            {
                builder.tempdir()
            }
        }
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
            bridge_endpoint: OnceLock::new(),
            bridge_scopes: std::sync::Mutex::new(HashMap::new()),
        });
        mcp::listen(&browser)?;
        Ok(browser)
    }
    pub(crate) fn set_preview_resources(
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

    pub(crate) async fn start_preview_recording_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
        recording_id: String,
        options: agent_protocol::preview::PreviewRecordingOptions,
    ) -> Result<agent_protocol::preview::PreviewRecordingStatus, String> {
        self.start_preview_recording_with_cancel_for_owner(
            owner,
            thread,
            tab_id,
            recording_id,
            options,
            CancellationToken::new(),
        )
        .await
    }

    pub(crate) async fn start_preview_recording_with_cancel_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
        recording_id: String,
        options: agent_protocol::preview::PreviewRecordingOptions,
        request_cancel: CancellationToken,
    ) -> Result<agent_protocol::preview::PreviewRecordingStatus, String> {
        if request_cancel.is_cancelled() {
            return Err("recording start was cancelled".to_owned());
        }
        options.validate()?;
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
            if !can_access_preview_tab(page, Some(owner), tab_id) {
                return Err("Preview tab belongs to another Host session".into());
            }
            let dimensions = page.viewport_for(tab_id);
            let profile_key = Self::tab_profile(page, tab_id)
                .ok_or_else(|| "Preview tab profile owner is missing".to_owned())?;
            let endpoint = state
                .preview_chromes
                .get(&profile_key)
                .ok_or_else(|| "browser profile is unavailable".to_owned())?
                .chrome
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
            recording_id,
            self.bridge_directory.path().join("recordings"),
            width,
            height,
            &protected_paths,
            options,
            cancel.clone(),
            self.stop.clone(),
        )?;
        let recording::StartResult {
            recording_id,
            started_at,
            artifact_path,
            externally_detached,
            startup,
            task,
            overlay,
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
                recording_id: recording_id.clone(),
                artifact_path,
                startup: startup_state.clone(),
                done: done.clone(),
                started_at: started_at.clone(),
                stopping: false,
                overlay,
            },
        );
        drop(recordings);
        drop(artifacts);
        let recordings = self.recordings.clone();
        let recording_artifacts = self.recording_artifacts.clone();
        let browser_state = self.state.clone();
        let preview = self.preview.get().cloned();
        let monitor_external_detached = externally_detached.clone();
        let monitor_key = key.clone();
        let monitor_tab_id = tab_id.to_owned();
        let monitor_thread = thread.to_owned();
        let monitor_recording_id = recording_id.clone();
        tokio::spawn(async move {
            let result = match task.await {
                Ok(result) => result,
                Err(error) => Err(format!("recording task terminated: {error}")),
            };
            let mut paths_to_remove = Vec::new();
            let mut invalidated_recordings = Vec::new();
            if let Ok(artifact) = &result {
                // Keep the artifact only while this monitor still owns the
                // active lifetime.  A late completion after close or a new
                // start on the same tab must not become the tab's artifact.
                let mut artifacts = recording_artifacts.lock().await;
                let current_lifetime_matches = recordings
                    .lock()
                    .await
                    .get(&monitor_key)
                    .is_some_and(|active| {
                        active.recording_id.as_str() == monitor_recording_id.as_str()
                            && artifact.recording_id.as_str() == monitor_recording_id.as_str()
                    });
                if current_lifetime_matches {
                    let entries = artifacts.entry(monitor_key.clone()).or_default();
                    entries.push(artifact.clone());
                    // One completed artifact per tab is enough for a duplicate
                    // stop or a client that is still displaying Save/Attach.
                    while entries.len() > 1 {
                        paths_to_remove.push(PathBuf::from(entries.remove(0).path));
                    }
                    let evicted =
                        prune_completed_recordings(&mut artifacts, MAX_RETAINED_RECORDINGS);
                    for (key, path) in evicted {
                        invalidated_recordings.push(key);
                        paths_to_remove.push(path);
                    }
                } else {
                    paths_to_remove.push(PathBuf::from(artifact.path.clone()));
                }
            }
            drop(recording_artifacts);
            if let Some(preview) = preview.as_ref() {
                for (thread, tab_id) in &invalidated_recordings {
                    if let Ok(thread_id) = agent_domain::ThreadId::new(thread.clone()) {
                        preview.recording_artifact_removed(&thread_id, tab_id);
                    }
                }
            }
            for path in paths_to_remove {
                let _ = tokio::fs::remove_file(path).await;
            }
            done.send_replace(Some(result));
            let externally_detached =
                monitor_external_detached.load(std::sync::atomic::Ordering::Acquire);
            if externally_detached {
                let mut state = browser_state.lock().await;
                forget_detached_preview_target(&mut state, &monitor_thread, &monitor_tab_id);
            }
            if let Some(preview) = preview.as_ref()
                && let Ok(thread_id) = agent_domain::ThreadId::new(monitor_thread.clone())
            {
                preview.recording_finished(&thread_id, &monitor_tab_id, &monitor_recording_id);
            }
            if externally_detached
                && let Some(preview) = preview.as_ref()
                && let Ok(thread_id) = agent_domain::ThreadId::new(monitor_thread.clone())
            {
                preview.close(&thread_id, Some(&monitor_tab_id));
            }
            let mut recordings = recordings.lock().await;
            if recordings
                .get(&monitor_key)
                .is_some_and(|active| active.recording_id.as_str() == monitor_recording_id.as_str())
            {
                recordings.remove(&monitor_key);
            }
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
            recording_id: recording_id.clone(),
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
            if preview.get(&thread_id, tab_id).is_err() {
                // MCP may start recording a live browser target immediately
                // after a Host reconnect, before PreviewList has rehydrated
                // its metadata owner.
                let _ = preview.open(
                    thread_id.clone(),
                    tab_id.to_owned(),
                    None,
                    PreviewViewportSetting::Fill,
                    PreviewAppearance::System,
                    PreviewZoom::X100,
                    None,
                );
            }
            if let Err(error) = preview.recording_started(thread_id, status.clone()) {
                self.cancel_recording(&key, &mut done_receiver, Duration::from_secs(5))
                    .await;
                self.discard_recording_artifact_path(&key, &artifact_path_for_failure)
                    .await;
                return Err(error);
            }
            if !self.recordings.lock().await.contains_key(&key) {
                if let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned()) {
                    preview.recording_finished(&thread_id, tab_id, &recording_id);
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

    pub(crate) async fn stop_preview_recording_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
        recording_id: &str,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        self.stop_preview_recording_with_timeout_for_owner(
            owner,
            thread,
            tab_id,
            Some(recording_id),
            Duration::from_secs(agent_protocol::preview::PREVIEW_RECORDING_MAX_DURATION_SECONDS),
            None,
        )
        .await
    }

    pub(crate) async fn stop_preview_recording_with_cancel_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
        recording_id: &str,
        request_cancel: CancellationToken,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        self.stop_preview_recording_with_timeout_for_owner(
            owner,
            thread,
            tab_id,
            Some(recording_id),
            Duration::from_secs(agent_protocol::preview::PREVIEW_RECORDING_MAX_DURATION_SECONDS),
            Some(request_cancel),
        )
        .await
    }

    async fn stop_preview_recording_with_timeout_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
        expected_recording_id: Option<&str>,
        timeout: Duration,
        request_cancel: Option<CancellationToken>,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        {
            let state = self.state.lock().await;
            let page = state
                .pages
                .get(thread)
                .ok_or_else(|| "preview thread was not found".to_owned())?;
            if !can_access_preview_tab(page, Some(owner), tab_id) {
                return Err("Preview tab belongs to another Host session".into());
            }
        }
        let key = (thread.to_owned(), tab_id.to_owned());
        let active = {
            let mut recordings = self.recordings.lock().await;
            match recordings.get_mut(&key) {
                Some(active) => {
                    if expected_recording_id.is_some_and(|expected| expected != active.recording_id)
                    {
                        return Err(format!(
                            "recording lifetime changed for preview tab {tab_id}"
                        ));
                    }
                    let initiate_stop = begin_recording_stop(&mut active.stopping);
                    Some((
                        active.cancel.clone(),
                        active.abort.clone(),
                        active.done.subscribe(),
                        active.startup.subscribe(),
                        initiate_stop,
                        active.recording_id.clone(),
                    ))
                }
                None => None,
            }
        };
        let Some((cancel, abort, mut done, mut startup, initiate_stop, recording_id)) = active
        else {
            return self
                .completed_recording(&key, expected_recording_id)
                .await
                .ok_or_else(|| format!("recording is not active for preview tab {tab_id}"));
        };
        if initiate_stop {
            let startup_result = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_recording_startup(&mut startup),
            )
            .await;
            if startup_result.is_err() {
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
        if result.recording_id.as_str() != recording_id.as_str() {
            return Err(format!(
                "recording completion identity did not match preview tab {tab_id}"
            ));
        }
        Ok(result)
    }

    async fn completed_recording(
        &self,
        key: &RecordingKey,
        expected_recording_id: Option<&str>,
    ) -> Option<RecordingArtifact> {
        self.recording_artifacts
            .lock()
            .await
            .get(key)
            .and_then(|artifacts| {
                artifacts
                    .iter()
                    .rev()
                    .find(|artifact| {
                        expected_recording_id
                            .is_none_or(|expected| expected == artifact.recording_id)
                    })
                    .cloned()
            })
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
        let mut artifacts = self.recording_artifacts.lock().await;
        let remove_key = if let Some(entries) = artifacts.get_mut(key) {
            entries.retain(|artifact| artifact.path.as_str() != target.as_str());
            entries.is_empty()
        } else {
            false
        };
        if remove_key {
            artifacts.remove(key);
        }
        drop(artifacts);
        // A cancelled start owns this freshly allocated path even if the
        // monitor completed and briefly offered it before cancellation was
        // observed.  The caller is intentionally discarding that output.
        let _ = tokio::fs::remove_file(path).await;
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
            let Some(active) = recordings.get_mut(key) else {
                return;
            };
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

    pub(crate) async fn active_recording_for_owner(
        &self,
        owner: &str,
        thread: &str,
    ) -> Result<(String, String), String> {
        let state = self.state.lock().await;
        let recordings = self.recordings.lock().await;
        let mut tabs = recordings
            .iter()
            .filter(|((scope_thread, tab_id), _active)| {
                scope_thread == thread
                    && state
                        .pages
                        .get(thread)
                        .is_some_and(|page| can_access_preview_tab(page, Some(owner), tab_id))
            })
            .map(|((_, tab), active)| (tab.clone(), active.recording_id.clone()));
        match (tabs.next(), tabs.next()) {
            (Some(recording), None) => Ok(recording),
            (None, _) => Err("no Preview recording is active for this conversation".into()),
            (Some(_), Some(_)) => {
                Err("multiple Preview recordings are active; specify tab_id".into())
            }
        }
    }

    pub(crate) async fn recording_id_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
    ) -> Result<String, String> {
        let state = self.state.lock().await;
        let page = state
            .pages
            .get(thread)
            .ok_or_else(|| "preview thread was not found".to_owned())?;
        if !can_access_preview_tab(page, Some(owner), tab_id) {
            return Err("Preview tab belongs to another Host session".into());
        }
        self.recordings
            .lock()
            .await
            .get(&(thread.to_owned(), tab_id.to_owned()))
            .map(|active| active.recording_id.clone())
            .ok_or_else(|| format!("recording is not active for preview tab {tab_id}"))
    }

    pub(crate) async fn preview_active_tab_for_owner(
        &self,
        owner: &str,
        thread: &str,
    ) -> Result<String, String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let page = state
            .pages
            .get(thread)
            .ok_or_else(|| "preview thread was not found".to_owned())?;
        if let Some(active) = page.selected_for_owner(Some(owner))
            && page.preview_tabs.contains(active)
            && can_access_preview_tab(page, Some(owner), active)
        {
            Ok(active.to_owned())
        } else {
            let mut owned = page
                .preview_tabs
                .iter()
                .filter(|tab_id| can_access_preview_tab(page, Some(owner), tab_id.as_str()));
            match (owned.next(), owned.next()) {
                (Some(tab_id), None) => Ok(tab_id.clone()),
                (None, _) => Err("preview tab is not open".to_owned()),
                (Some(_), Some(_)) => {
                    Err("multiple Preview tabs are open; specify tab_id".to_owned())
                }
            }
        }
    }

    pub(crate) async fn validate_preview_tab_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let page = state
            .pages
            .get(thread)
            .ok_or_else(|| "preview thread was not found".to_owned())?;
        if !page.preview_tabs.contains(tab_id) {
            return Err("preview tab was not found".to_owned());
        }
        if !can_access_preview_tab(page, Some(owner), tab_id) {
            return Err("Preview tab belongs to another Host session".to_owned());
        }
        Ok(())
    }

    pub fn provider_config(&self, thread: &str) -> Result<serde_json::Value, String> {
        let endpoint = self
            .bridge_endpoint
            .get()
            .cloned()
            .ok_or_else(|| "browser bridge is not listening".to_owned())?;
        if thread.is_empty() || thread.len() > 8192 {
            return Err("browser scope is required".into());
        }
        let token = {
            let mut scopes = self
                .bridge_scopes
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some((token, _)) = scopes.iter().find(|(_, scope)| scope.thread == thread) {
                token.clone()
            } else {
                let token = uuid::Uuid::new_v4().simple().to_string();
                // The provider browser is the collaborative browser surface
                // for this conversation.  It must use the shared owner so
                // the agent and human see the same tabs and cookie jar;
                // client-local Preview profiles remain isolated by their
                // profile owner when explicitly selected.
                let owner = COLLABORATIVE_BROWSER_OWNER.to_owned();
                scopes.insert(
                    token.clone(),
                    BrowserBridgeScope {
                        owner: owner.clone(),
                        thread: thread.to_owned(),
                    },
                );
                token
            }
        };
        Ok(serde_json::json!({
            "command":std::env::current_exe().map_err(|e| e.to_string())?,
            "args":["browser-mcp", "--socket", endpoint, "--thread", thread],
            "env":{"AGENT_TOOLS_TOKEN":token},
        }))
    }

    pub(crate) fn bridge_scope(&self, token: &str) -> Option<BrowserBridgeScope> {
        self.bridge_scopes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(token)
            .cloned()
    }
    pub(crate) fn set_bridge_endpoint(&self, endpoint: String) -> Result<(), String> {
        self.bridge_endpoint
            .set(endpoint)
            .map_err(|_| "browser bridge endpoint already configured".to_owned())
    }
    fn socket_path(&self) -> PathBuf {
        self.bridge_directory.path().join("bridge.sock")
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let keys = self
            .recordings
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            let (mut done, mut startup) = {
                let mut recordings = self.recordings.lock().await;
                let Some(active) = recordings.get_mut(&key) else {
                    continue;
                };
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
        let (chrome, preview_chromes) = {
            let mut state = self.state.lock().await;
            (
                state.chrome.take(),
                std::mem::take(&mut state.preview_chromes),
            )
        };
        if let Some(chrome) = chrome {
            chrome.shutdown().await;
        }
        for (_, preview) in preview_chromes {
            preview.shutdown().await;
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
                let detached = page
                    .tabs
                    .iter()
                    .filter(|tab_id| Self::tab_profile(page, tab_id).is_none())
                    .cloned()
                    .collect::<Vec<_>>();
                for tab_id in detached {
                    page.tabs.retain(|id| id != &tab_id);
                    page.viewports.remove(&tab_id);
                    page.preview_tabs.remove(&tab_id);
                    page.preview_profiles.remove(&tab_id);
                    page.preview_profile_owners.remove(&tab_id);
                    page.preview_settings.remove(&tab_id);
                }
                if !page.tabs.contains(&page.active) {
                    page.active = page.tabs.last().cloned().unwrap_or_default();
                }
                page.retain_valid_selections();
            }
        }
        if state.chrome.is_none() {
            state.chrome = Some(cdp::Chrome::launch(&self.profile, &self.executable).await?);
        }
        let broken_profiles = state
            .preview_chromes
            .iter()
            .filter(|(_, preview)| preview.chrome.broken)
            .map(|(profile_id, _)| profile_id.clone())
            .collect::<Vec<_>>();
        for profile_key in &broken_profiles {
            for page in state.pages.values_mut() {
                let detached = page
                    .preview_profiles
                    .iter()
                    .filter(|(tab_id, _)| {
                        Self::tab_profile(page, tab_id) == Some(profile_key.clone())
                    })
                    .map(|(tab_id, _)| tab_id.clone())
                    .collect::<Vec<_>>();
                for tab_id in detached {
                    page.tabs.retain(|id| id != &tab_id);
                    page.viewports.remove(&tab_id);
                    page.preview_tabs.remove(&tab_id);
                    page.preview_profiles.remove(&tab_id);
                    page.preview_profile_owners.remove(&tab_id);
                    page.preview_settings.remove(&tab_id);
                }
                if !page.tabs.contains(&page.active) {
                    page.active = page.tabs.last().cloned().unwrap_or_default();
                }
                page.retain_valid_selections();
            }
        }
        for profile_key in broken_profiles {
            if let Some(preview) = state.preview_chromes.remove(&profile_key) {
                preview.shutdown().await;
            }
        }
        if !state.pages.contains_key(thread) && state.pages.len() >= 32 {
            return Err("ブラウザを開いている会話が上限に達しました。".into());
        }
        let targets = Self::all_targets(state).await?;
        let create_default_tab = state
            .pages
            .get(thread)
            .is_none_or(|page| page.tabs.is_empty());
        let default_tab = if create_default_tab {
            Some(
                state
                    .chrome
                    .as_mut()
                    .expect("default browser was initialized above")
                    .create()
                    .await?,
            )
        } else {
            None
        };
        let needs_default_tab = {
            let page = state.pages.entry(thread.into()).or_default();
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
                    .map(|target| {
                        let profile = target
                            .opener_id
                            .as_ref()
                            .and_then(|opener| page.preview_profiles.get(opener).cloned())
                            .flatten();
                        let owner = target
                            .opener_id
                            .as_ref()
                            .and_then(|opener| page.preview_profile_owners.get(opener).cloned());
                        (target.target_id.clone(), profile, owner)
                    })
                    .collect();
                if added.is_empty() {
                    break;
                }
                page.active = added.last().unwrap().0.clone();
                for (tab_id, profile, owner) in added {
                    page.preview_profiles.insert(tab_id.clone(), profile);
                    if let Some(owner) = owner {
                        page.preview_profile_owners.insert(tab_id.clone(), owner);
                    }
                    page.tabs.push(tab_id);
                }
            }
            page.tabs
                .retain(|id| targets.iter().any(|target| &target.target_id == id));
            page.viewports
                .retain(|id, _| targets.iter().any(|target| &target.target_id == id));
            page.preview_tabs
                .retain(|id| targets.iter().any(|target| &target.target_id == id));
            page.preview_profiles
                .retain(|id, _| targets.iter().any(|target| &target.target_id == id));
            page.preview_profile_owners
                .retain(|id, _| targets.iter().any(|target| &target.target_id == id));
            page.preview_settings
                .retain(|id, _| targets.iter().any(|target| &target.target_id == id));
            let needs_default_tab = match default_tab {
                Some(id) => {
                    page.tabs.push(id.clone());
                    page.active = id;
                    page.viewports.insert(page.active.clone(), (WIDTH, HEIGHT));
                    false
                }
                None if page.tabs.is_empty() => true,
                None => {
                    if !page.tabs.contains(&page.active) {
                        page.active = page.tabs.last().unwrap().clone();
                    }
                    false
                }
            };
            if !needs_default_tab {
                page.retain_valid_selections();
            }
            needs_default_tab
        };
        if needs_default_tab {
            // A conversation can retain metadata after every tab was closed
            // externally. Reconcile first, then create the one default page
            // needed to keep the browser operation usable.
            let id = state
                .chrome
                .as_mut()
                .expect("default browser was initialized above")
                .create()
                .await?;
            let page = state.pages.get_mut(thread).expect("browser page exists");
            page.tabs.push(id.clone());
            page.active = id.clone();
            page.viewports.insert(id, (WIDTH, HEIGHT));
            page.retain_valid_selections();
        }
        Ok(())
    }

    pub async fn request_for_owner(
        &self,
        owner: &str,
        request: &BrowserRequest,
    ) -> Result<BrowserFrame, String> {
        let thread = request.thread_id.to_string();
        let mut state = self.state.lock().await;
        self.ensure(&mut state, &thread).await?;
        let active = {
            let page = state.pages.get_mut(&thread).unwrap();
            let preferred = (!matches!(request.action, BrowserAction::SelectTab { .. }))
                .then_some(request.tab_id.as_str());
            Self::activate_tab_for_owner(page, Some(owner), preferred)?
        };
        let viewport = state.pages[&thread].viewport_for(&active);
        request.validate_for_viewport(viewport.0, viewport.1)?;
        validate_tab(&active, &request.tab_id, &request.action)?;
        Self::action(&mut state, owner, &thread, &request.action).await?;
        let viewport = state.pages[&thread].viewport_for(&request.tab_id);
        drop(state);
        self.publish_recording_input(&thread, &request.tab_id, &request.action, viewport)
            .await;
        let mut state = self.state.lock().await;
        let preferred = match &request.action {
            BrowserAction::SelectTab { id } => Some(id.as_str()),
            _ => Some(request.tab_id.as_str()),
        };
        let mut frame = Self::frame(&mut state, &thread, Some(owner), preferred).await?;
        if frame.image_id == request.image_id {
            frame.image.clear();
        }
        self.report_preview_frame(&thread, &frame);
        Ok(frame)
    }

    pub(super) async fn agent_for_owner(
        &self,
        owner: &str,
        thread: &str,
        action: BrowserAction,
    ) -> Result<BrowserFrame, String> {
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let viewport = {
            let page = state.pages.get_mut(thread).unwrap();
            let active = Self::activate_tab_for_owner(page, Some(owner), None)?;
            page.viewport_for(&active)
        };
        action.validate_for_viewport(viewport.0, viewport.1)?;
        Self::action(&mut state, owner, thread, &action).await?;
        let tab_id = {
            let page = state.pages.get_mut(thread).unwrap();
            Self::activate_tab_for_owner(page, Some(owner), None)?
        };
        let viewport = state.pages[thread].viewport_for(&tab_id);
        drop(state);
        self.publish_recording_input(thread, &tab_id, &action, viewport)
            .await;
        let mut state = self.state.lock().await;
        let frame = Self::frame(&mut state, thread, Some(owner), None).await?;
        drop(state);
        self.report_preview_frame(thread, &frame);
        Ok(frame)
    }

    async fn publish_recording_input(
        &self,
        thread: &str,
        tab_id: &str,
        action: &BrowserAction,
        viewport: (u32, u32),
    ) {
        let overlay = self
            .recordings
            .lock()
            .await
            .get(&(thread.to_owned(), tab_id.to_owned()))
            .map(|recording| recording.overlay.clone());
        let Some(overlay) = overlay else { return };
        match action {
            BrowserAction::Click { x, y } => recording::apply_input(
                &overlay,
                recording::InputEvent::Pointer {
                    x: *x,
                    y: *y,
                    width: viewport.0,
                    height: viewport.1,
                },
            ),
            BrowserAction::Key { key } => {
                let label = browser_key_label(*key).to_owned();
                recording::apply_input(
                    &overlay,
                    recording::InputEvent::Key {
                        label: label.clone(),
                        down: true,
                    },
                );
                recording::apply_input(&overlay, recording::InputEvent::Key { label, down: false });
            }
            _ => {}
        }
    }

    /// Creates a Host browser tab for the Preview surface and returns its
    /// first frame. Existing agent browser tabs remain in the same conversation
    /// scope and are never replaced.
    // Keep the tab identity, viewport, client owner, and profile explicit at
    // this resource boundary; bundling them would hide which owner controls
    // the browser partition and rendered dimensions.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_preview_tab_for_owner(
        &self,
        owner: &str,
        thread: &str,
        url: Option<&str>,
        viewport: PreviewViewportSetting,
        appearance: PreviewAppearance,
        zoom: PreviewZoom,
        rendered_size: Option<PreviewRenderedViewportSize>,
        profile_id: Option<String>,
    ) -> Result<BrowserFrame, String> {
        viewport.validate()?;
        if let Some(size) = rendered_size {
            size.validate()?;
        }
        if let Some(profile_id) = &profile_id {
            agent_protocol::preview::validate_profile_id(profile_id)?;
        }
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let resource_viewport = state.pages[thread].viewport();
        let (width, height) = viewport
            .dimensions()
            .or_else(|| rendered_size.map(|size| (size.width, size.height)))
            .unwrap_or(resource_viewport);
        let profile_id_key = Self::preview_profile_key(profile_id.as_deref()).to_owned();
        let id = {
            self.ensure_preview_chrome(&mut state, owner, &profile_id_key)
                .await?
                .chrome
                .create()
                .await?
        };
        {
            let page = state.pages.get_mut(thread).unwrap();
            page.tabs.push(id.clone());
            page.active = id.clone();
            page.set_selected_for_owner(Some(owner), id.clone());
            page.viewports.insert(id.clone(), (width, height));
            page.preview_tabs.insert(page.active.clone());
            page.preview_profiles
                .insert(page.active.clone(), profile_id.clone());
            page.preview_profile_owners
                .insert(page.active.clone(), owner.to_owned());
            page.preview_settings
                .insert(page.active.clone(), (appearance, zoom));
        }
        let session = {
            let chrome = Self::chrome_for_profile(&mut state, Some(owner), Some(&profile_id_key))?;
            chrome.attach(&id, width, height).await?
        };
        {
            let chrome = Self::chrome_for_profile(&mut state, Some(owner), Some(&profile_id_key))?;
            chrome.set_appearance(&session, appearance).await?;
            chrome.set_zoom(&session, zoom).await?;
        }
        if let Some(url) = url {
            let action = BrowserAction::Navigate {
                url: url.to_owned(),
            };
            Self::action(&mut state, owner, thread, &action).await?;
        }
        Self::frame(&mut state, thread, Some(owner), Some(&id)).await
    }

    /// Clears the cookies, cache and origin storage for one browser profile
    /// owned by this Host. The profile row remains the caller's responsibility
    /// and must only be removed after this operation returns successfully.
    pub(crate) async fn clear_preview_profile_for_owner(
        &self,
        owner: &str,
        profile_id: &str,
    ) -> Result<(), String> {
        agent_protocol::preview::validate_profile_id(profile_id)?;
        let mut state = self.state.lock().await;
        let profile_id = Self::preview_profile_key(Some(profile_id)).to_owned();
        let result = self
            .ensure_preview_chrome(&mut state, owner, &profile_id)
            .await?
            .chrome
            .clear_profile_data()
            .await
            .map(|_| ());
        let dispose_incognito = result.is_ok()
            && profile_id == agent_protocol::preview::INCOGNITO_PREVIEW_PROFILE_ID
            && !state.pages.values().any(|page| {
                page.preview_profiles.iter().any(|(tab_id, profile)| {
                    page.preview_profile_owners.get(tab_id).map(String::as_str) == Some(owner)
                        && profile.as_deref() == Some(profile_id.as_str())
                })
            });
        let incognito_chrome = if dispose_incognito {
            state.preview_chromes.remove(&PreviewChromeKey {
                owner: owner.to_owned(),
                profile_id: profile_id.clone(),
            })
        } else {
            None
        };
        drop(state);
        if let Some(chrome) = incognito_chrome {
            chrome.shutdown().await;
        }
        result
    }

    pub(crate) async fn resize_preview_tab_for_owner(
        &self,
        owner: &str,
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
            if !can_access_preview_tab(page, Some(owner), tab_id) {
                return Err("Preview tab belongs to another Host session".into());
            }
            page.set_selected_for_owner(Some(owner), tab_id.to_owned());
            page.viewports.insert(tab_id.to_owned(), dimensions);
        }
        Self::frame(&mut state, thread, Some(owner), Some(tab_id)).await
    }

    pub(crate) async fn set_preview_appearance_for_owner(
        &self,
        owner: &str,
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
            if !can_access_preview_tab(page, Some(owner), tab_id) {
                return Err("Preview tab belongs to another Host session".into());
            }
            page.set_selected_for_owner(Some(owner), tab_id.to_owned());
            page.viewport_for(tab_id)
        };
        let active = tab_id.to_owned();
        let profile_key = state
            .pages
            .get(thread)
            .and_then(|page| Self::tab_profile(page, tab_id));
        {
            let chrome = Self::chrome_for_key(&mut state, profile_key)?;
            let session = chrome.attach(&active, viewport.0, viewport.1).await?;
            chrome.set_appearance(&session, appearance).await?;
        }
        state
            .pages
            .get_mut(thread)
            .unwrap()
            .preview_settings
            .entry(active.clone())
            .and_modify(|settings| settings.0 = appearance);
        Self::frame(&mut state, thread, Some(owner), Some(tab_id)).await
    }

    pub(crate) async fn set_preview_zoom_for_owner(
        &self,
        owner: &str,
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
            if !can_access_preview_tab(page, Some(owner), tab_id) {
                return Err("Preview tab belongs to another Host session".into());
            }
            page.set_selected_for_owner(Some(owner), tab_id.to_owned());
            page.viewport_for(tab_id)
        };
        let active = tab_id.to_owned();
        let profile_key = state
            .pages
            .get(thread)
            .and_then(|page| Self::tab_profile(page, tab_id));
        {
            let chrome = Self::chrome_for_key(&mut state, profile_key)?;
            let session = chrome.attach(&active, viewport.0, viewport.1).await?;
            chrome.set_zoom(&session, zoom).await?;
        }
        state
            .pages
            .get_mut(thread)
            .unwrap()
            .preview_settings
            .entry(active.clone())
            .and_modify(|settings| settings.1 = zoom);
        Self::frame(&mut state, thread, Some(owner), Some(tab_id)).await
    }

    pub(crate) async fn close_preview_tab_with_cancel_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
        request_cancel: CancellationToken,
    ) -> Result<(), String> {
        if request_cancel.is_cancelled() {
            return Err(format!("closing Preview tab {tab_id} was cancelled"));
        }
        let key = (thread.to_owned(), tab_id.to_owned());
        let active_recording_id = self
            .recordings
            .lock()
            .await
            .get(&key)
            .map(|active| active.recording_id.clone());
        let stop_result = if let Some(recording_id) = active_recording_id.as_deref() {
            let result = self
                .stop_preview_recording_with_timeout_for_owner(
                    owner,
                    thread,
                    tab_id,
                    Some(recording_id),
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
        if let Some(Err(error)) = stop_result.as_ref() {
            // Keep the tab and its recording owner available when finalization
            // failed. Closing here would discard the only retryable session
            // while the client still has no usable artifact.
            return Err(format!(
                "Preview tab was not closed because recording stop failed: {error}"
            ));
        }
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        {
            let page = state.pages.get(thread).unwrap();
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
            if !can_access_preview_tab(page, Some(owner), tab_id) {
                return Err("Preview tab belongs to another Host session".into());
            }
        }
        let profile_to_dispose = state
            .pages
            .get(thread)
            .and_then(|page| page.preview_profiles.get(tab_id))
            .cloned()
            .flatten();
        let profile_key = state
            .pages
            .get(thread)
            .and_then(|page| Self::tab_profile(page, tab_id))
            .ok_or_else(|| "Preview tab profile owner is missing".to_owned())?;
        let chrome = Self::chrome_for_key(&mut state, Some(profile_key.clone()))?;
        let close_result = tokio::select! {
            result = chrome.close_target(tab_id) => result,
            _ = request_cancel.cancelled() => {
                return Err(format!("closing Preview tab {tab_id} was cancelled"));
            }
        };
        close_result?;
        let page = state.pages.get_mut(thread).unwrap();
        page.tabs.retain(|id| id != tab_id);
        page.viewports.remove(tab_id);
        page.preview_tabs.remove(tab_id);
        page.preview_profiles.remove(tab_id);
        page.preview_profile_owners.remove(tab_id);
        page.preview_settings.remove(tab_id);
        page.remove_from_selections(tab_id);
        let dispose_incognito = profile_to_dispose.as_deref()
            == Some(agent_protocol::preview::INCOGNITO_PREVIEW_PROFILE_ID)
            && !state.pages.values().any(|page| {
                page.preview_profiles.iter().any(|(tab_id, profile)| {
                    page.preview_profile_owners.get(tab_id).map(String::as_str)
                        == Some(profile_key.owner.as_str())
                        && profile.as_deref()
                            == Some(agent_protocol::preview::INCOGNITO_PREVIEW_PROFILE_ID)
                })
            });
        let incognito_chrome = if dispose_incognito {
            state.preview_chromes.remove(&PreviewChromeKey {
                owner: profile_key.owner,
                profile_id: agent_protocol::preview::INCOGNITO_PREVIEW_PROFILE_ID.to_owned(),
            })
        } else {
            None
        };
        drop(state);
        if let Some(chrome) = incognito_chrome {
            chrome.shutdown().await;
        }
        if stop_result.is_none() || stop_result.as_ref().is_some_and(|result| result.is_ok()) {
            self.clear_recording_artifacts(&key).await;
        }
        if let Some(preview) = self.preview.get()
            && let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned())
        {
            preview.close(&thread_id, Some(tab_id));
        }
        Ok(())
    }

    pub(crate) async fn close_preview_tab_for_owner(
        &self,
        owner: &str,
        thread: &str,
        tab_id: &str,
    ) -> Result<(), String> {
        self.close_preview_tab_with_cancel_for_owner(
            owner,
            thread,
            tab_id,
            CancellationToken::new(),
        )
        .await
    }

    /// Returns the tabs currently owned by the shared browser page.  The RPC
    /// Preview manager remains the authoritative metadata owner; this view is
    /// used only by the provider bridge's preview discovery tool.
    pub(crate) async fn preview_list_for_owner(
        &self,
        owner: &str,
        thread: &str,
    ) -> Result<agent_protocol::preview::PreviewListResult, String> {
        let thread_id = agent_domain::ThreadId::new(thread.to_owned())
            .map_err(|_| "browser scope is invalid".to_owned())?;
        let (mut browser_result, detached_preview_tabs, live_preview_tabs, owned_preview_tabs) = {
            let mut state = self.state.lock().await;
            self.ensure(&mut state, thread).await?;
            let (tabs, viewports, settings) = {
                let page = state.pages.get(thread).unwrap();
                (
                    page.preview_tabs
                        .iter()
                        .filter(|tab_id| can_access_preview_tab(page, Some(owner), tab_id.as_str()))
                        .cloned()
                        .collect::<HashSet<_>>(),
                    page.viewports.clone(),
                    page.preview_settings.clone(),
                )
            };
            let profiles = state.pages[thread]
                .preview_profiles
                .iter()
                .filter(|(tab_id, _)| tabs.contains(*tab_id))
                .map(|(tab_id, profile)| (tab_id.clone(), profile.clone()))
                .collect::<HashMap<_, _>>();
            let targets = Self::all_targets(&mut state).await?;
            let live_targets = targets
                .iter()
                .map(|target| target.target_id.as_str())
                .collect::<HashSet<_>>();
            let detached_preview_tabs = {
                let page = state.pages.get_mut(thread).unwrap();
                let detached = page
                    .preview_tabs
                    .iter()
                    .filter(|tab_id| {
                        tabs.contains(*tab_id) && !live_targets.contains(tab_id.as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for tab_id in &detached {
                    page.tabs.retain(|id| id != tab_id);
                    page.viewports.remove(tab_id);
                    page.preview_tabs.remove(tab_id);
                    page.preview_profiles.remove(tab_id);
                    page.preview_profile_owners.remove(tab_id);
                    page.preview_settings.remove(tab_id);
                }
                if !live_targets.contains(page.active.as_str()) {
                    page.active = page.tabs.last().cloned().unwrap_or_default();
                }
                page.retain_valid_selections();
                detached
            };
            let live_preview_tabs = {
                let page = state.pages.get(thread).unwrap();
                targets
                    .iter()
                    .filter(|target| {
                        page.preview_tabs.contains(&target.target_id)
                            && tabs.contains(&target.target_id)
                    })
                    .map(|target| target.target_id.clone())
                    .collect::<HashSet<_>>()
            };
            for tab_id in &detached_preview_tabs {
                if let Some(chrome) = state.chrome.as_mut() {
                    chrome.forget_target(tab_id);
                }
                for preview in state.preview_chromes.values_mut() {
                    preview.chrome.forget_target(tab_id);
                }
            }
            let sessions = targets
                .into_iter()
                .filter(|target| tabs.contains(&target.target_id))
                .map(|target| {
                    let tab_id = target.target_id.clone();
                    let (width, height) =
                        viewports.get(&tab_id).copied().unwrap_or((WIDTH, HEIGHT));
                    let (appearance, zoom) = settings.get(&tab_id).copied().unwrap_or((
                        agent_protocol::preview::PreviewAppearance::System,
                        agent_protocol::preview::PreviewZoom::X100,
                    ));
                    agent_protocol::preview::PreviewSessionSnapshot {
                        thread_id: thread_id.clone(),
                        tab_id: tab_id.clone(),
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
                        profile_id: profiles.get(&tab_id).cloned().flatten(),
                        updated_at: String::new(),
                    }
                })
                .collect();
            (
                agent_protocol::preview::PreviewListResult {
                    sessions,
                    recordings: Vec::new(),
                    invalidated_recordings: Vec::new(),
                    local_servers: Vec::new(),
                    scanned_at: String::new(),
                    server_epoch: String::new(),
                    revision: 0,
                    scanner_epoch: String::new(),
                    scanner_revision: 0,
                },
                detached_preview_tabs,
                live_preview_tabs,
                tabs,
            )
        };
        let live_tab_ids = browser_result
            .sessions
            .iter()
            .map(|session| session.tab_id.clone())
            .collect::<HashSet<_>>();
        browser_result.recordings = self
            .recordings
            .lock()
            .await
            .iter()
            .filter(|((scope, tab_id), _)| scope == thread && live_tab_ids.contains(tab_id))
            .map(
                |((_, tab_id), active)| agent_protocol::preview::PreviewRecordingStatus {
                    tab_id: tab_id.clone(),
                    recording_id: active.recording_id.clone(),
                    recording: true,
                    started_at: Some(active.started_at.clone()),
                },
            )
            .collect();
        let (Some(preview), Some(preview_ports), Some(terminals)) = (
            self.preview.get(),
            self.preview_ports.get(),
            self.terminals.get(),
        ) else {
            return Ok(browser_result);
        };
        if let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned()) {
            for tab_id in detached_preview_tabs {
                preview.close(&thread_id, Some(&tab_id));
            }
        }
        preview_ports.set_terminal_owners(terminals.preview_process_owners());
        let discovered = preview_ports
            .scan_snapshot(&[], &terminals.summaries_now())
            .await?;
        let mut result = preview.list(&thread_id);
        let stale_metadata_tabs = result
            .sessions
            .iter()
            .filter(|session| {
                owned_preview_tabs.contains(&session.tab_id)
                    && !live_preview_tabs.contains(&session.tab_id)
            })
            .map(|session| session.tab_id.clone())
            .collect::<Vec<_>>();
        for tab_id in stale_metadata_tabs {
            preview.close(&thread_id, Some(&tab_id));
        }
        result = preview.list(&thread_id);
        result
            .sessions
            .retain(|session| live_preview_tabs.contains(&session.tab_id));
        result
            .recordings
            .retain(|recording| live_preview_tabs.contains(&recording.tab_id));
        result
            .invalidated_recordings
            .retain(|tab_id| live_preview_tabs.contains(tab_id));
        let known_ids = result
            .sessions
            .iter()
            .map(|session| session.tab_id.as_str())
            .collect::<HashSet<_>>();
        for session in browser_result
            .sessions
            .iter()
            .filter(|session| !known_ids.contains(session.tab_id.as_str()))
        {
            // Browser tabs can outlive the metadata owner across a Host
            // reconnect. Rehydrate the owner before clients issue recording
            // or navigation calls against the still-live target.
            let url = match &session.nav_status {
                agent_protocol::preview::PreviewNavStatus::Idle => None,
                agent_protocol::preview::PreviewNavStatus::Loading { url, .. }
                | agent_protocol::preview::PreviewNavStatus::Success { url, .. }
                | agent_protocol::preview::PreviewNavStatus::LoadFailed { url, .. } => {
                    Some(url.as_str())
                }
            };
            if preview
                .open(
                    thread_id.clone(),
                    session.tab_id.clone(),
                    url,
                    session.viewport,
                    session.appearance,
                    session.zoom,
                    session.profile_id.clone(),
                )
                .is_ok()
            {
                let _ = preview.report_status(
                    &thread_id,
                    &session.tab_id,
                    session.nav_status.clone(),
                    session.can_go_back,
                    session.can_go_forward,
                );
            }
        }
        result = preview.list(&thread_id);
        result
            .sessions
            .retain(|session| live_preview_tabs.contains(&session.tab_id));
        result
            .recordings
            .retain(|recording| live_tab_ids.contains(&recording.tab_id));
        result
            .invalidated_recordings
            .retain(|tab_id| live_tab_ids.contains(tab_id));
        let metadata = result
            .sessions
            .into_iter()
            .map(|session| (session.tab_id.clone(), session))
            .collect::<HashMap<_, _>>();
        for session in &mut browser_result.sessions {
            if let Some(previous) = metadata.get(&session.tab_id) {
                session.viewport = previous.viewport;
                session.zoom = previous.zoom;
                session.appearance = previous.appearance;
            }
        }
        result.sessions = browser_result.sessions;
        result.recordings = browser_result.recordings;
        result.local_servers = discovered.servers;
        result.scanned_at = discovered.scanned_at;
        result.scanner_epoch = discovered.epoch;
        result.scanner_revision = discovered.revision;
        Ok(result)
    }

    async fn action(
        state: &mut State,
        owner: &str,
        thread: &str,
        action: &BrowserAction,
    ) -> Result<(), String> {
        if let BrowserAction::SelectTab { id } = action {
            let page = state.pages.get_mut(thread).unwrap();
            if !page.tabs.contains(id) {
                return Err("この会話のタブではありません。".into());
            }
            if !can_access_preview_tab(page, Some(owner), id) {
                return Err("Preview tab belongs to another Host session".into());
            }
            Self::activate_tab_for_owner(page, Some(owner), Some(id))?;
            return Ok(());
        }
        let active = {
            let page = state.pages.get_mut(thread).unwrap();
            Self::activate_tab_for_owner(page, Some(owner), None)?
        };
        if matches!(action, BrowserAction::Read) {
            return Ok(());
        }
        let (active, viewport, profile_key) = {
            let page = state.pages.get(thread).unwrap();
            let viewport = page.viewport_for(&active);
            let profile_key = Self::tab_profile(page, &active);
            (active, viewport, profile_key)
        };
        let chrome = match profile_key {
            Some(key) => state
                .preview_chromes
                .get_mut(&key)
                .map(|preview| &mut preview.chrome)
                .ok_or_else(|| format!("browser profile {} is unavailable", key.profile_id))?,
            None => state
                .chrome
                .as_mut()
                .ok_or_else(|| "browser is unavailable".to_owned())?,
        };
        let session = chrome.attach(&active, viewport.0, viewport.1).await?;
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

    async fn frame(
        state: &mut State,
        thread: &str,
        owner: Option<&str>,
        preferred: Option<&str>,
    ) -> Result<BrowserFrame, String> {
        let (active, viewport, tabs, profile_key) = {
            let page = state.pages.get_mut(thread).unwrap();
            let active = Self::activate_tab_for_owner(page, owner, preferred)?;
            let viewport = page.viewport_for(&active);
            let profile_key = Self::tab_profile(page, &active);
            (active.clone(), viewport, page.tabs.clone(), profile_key)
        };
        let (image, dialog) = {
            let chrome = match profile_key {
                Some(key) => state
                    .preview_chromes
                    .get_mut(&key)
                    .map(|preview| &mut preview.chrome)
                    .ok_or_else(|| format!("browser profile {} is unavailable", key.profile_id))?,
                None => state
                    .chrome
                    .as_mut()
                    .ok_or_else(|| "browser is unavailable".to_owned())?,
            };
            let session = chrome.attach(&active, viewport.0, viewport.1).await?;
            let image = chrome.screenshot(&session).await?;
            let dialog = chrome.dialog(&session);
            (image, dialog)
        };
        let image_id = base64::engine::general_purpose::STANDARD
            .encode(ring::digest::digest(&ring::digest::SHA256, &image));
        let targets = Self::all_targets(state).await?;
        Ok(BrowserFrame {
            tabs: targets
                .into_iter()
                .filter(|target| {
                    tabs.contains(&target.target_id)
                        && owner.is_none_or(|owner| {
                            state.pages.get(thread).is_none_or(|page| {
                                can_access_preview_tab(page, Some(owner), &target.target_id)
                            })
                        })
                })
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
            dialog,
        })
    }

    fn activate_tab_for_owner(
        page: &mut Page,
        owner: Option<&str>,
        preferred: Option<&str>,
    ) -> Result<String, String> {
        let authorized = |tab_id: &str| {
            page.tabs.iter().any(|id| id == tab_id) && can_access_preview_tab(page, owner, tab_id)
        };
        let owned_preview = || {
            owner.and_then(|owner| {
                page.tabs.iter().rev().find(|tab_id| {
                    page.preview_tabs.contains(tab_id.as_str())
                        && can_access_preview_tab(page, Some(owner), tab_id.as_str())
                })
            })
        };
        let selected = owner.and_then(|owner| page.selected_for_owner(Some(owner)));
        let active = preferred
            .filter(|tab_id| authorized(tab_id))
            .or_else(|| selected.filter(|tab_id| authorized(tab_id)))
            .or_else(|| owned_preview().map(String::as_str))
            .or_else(|| {
                (!owner.is_some() && authorized(&page.active)).then_some(page.active.as_str())
            })
            .or_else(|| {
                page.tabs
                    .iter()
                    .rev()
                    .find(|tab_id| authorized(tab_id.as_str()))
                    .map(String::as_str)
            })
            .map(str::to_owned)
            .ok_or_else(|| "no browser tab is available for this Host session".to_owned())?;
        page.set_selected_for_owner(owner, active.clone());
        Ok(active)
    }
}

fn forget_detached_preview_target(state: &mut State, thread: &str, tab_id: &str) -> bool {
    if let Some(chrome) = state.chrome.as_mut() {
        chrome.forget_target(tab_id);
    }
    for preview in state.preview_chromes.values_mut() {
        preview.chrome.forget_target(tab_id);
    }
    let Some(page) = state.pages.get_mut(thread) else {
        return false;
    };
    let known = page.tabs.iter().any(|id| id == tab_id) || page.preview_tabs.contains(tab_id);
    page.tabs.retain(|id| id != tab_id);
    page.viewports.remove(tab_id);
    page.preview_tabs.remove(tab_id);
    page.preview_profiles.remove(tab_id);
    page.preview_profile_owners.remove(tab_id);
    page.preview_settings.remove(tab_id);
    page.remove_from_selections(tab_id);
    known
}

fn prune_completed_recordings(
    artifacts: &mut HashMap<RecordingKey, Vec<RecordingArtifact>>,
    maximum: usize,
) -> Vec<(RecordingKey, PathBuf)> {
    let mut removed = Vec::new();
    while artifacts.values().map(Vec::len).sum::<usize>() > maximum {
        let Some((key, index, _)) = artifacts
            .iter()
            .flat_map(|(key, entries)| {
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, artifact)| (key.clone(), index, artifact.created_at.clone()))
            })
            .min_by(|left, right| {
                left.2
                    .cmp(&right.2)
                    .then_with(|| left.0.cmp(&right.0))
                    .then_with(|| left.1.cmp(&right.1))
            })
        else {
            break;
        };
        let (path, empty) = {
            let entries = artifacts
                .get_mut(&key)
                .expect("the selected recording key remains owned");
            let path = PathBuf::from(entries.remove(index).path);
            (path, entries.is_empty())
        };
        removed.push((key.clone(), path));
        if empty {
            artifacts.remove(&key);
        }
    }
    removed
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

fn browser_key_label(key: agent_protocol::browser::BrowserKey) -> &'static str {
    use agent_protocol::browser::BrowserKey;
    match key {
        BrowserKey::Enter => "Enter",
        BrowserKey::Tab => "Tab",
        BrowserKey::Backspace => "Backspace",
        BrowserKey::Escape => "Escape",
        BrowserKey::ArrowUp => "ArrowUp",
        BrowserKey::ArrowDown => "ArrowDown",
        BrowserKey::ArrowLeft => "ArrowLeft",
        BrowserKey::ArrowRight => "ArrowRight",
        BrowserKey::SelectAll => "SelectAll",
    }
}

#[cfg(test)]
mod tests;
