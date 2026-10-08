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
};
use tokio::sync::Mutex;
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

struct ActiveRecording {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<Result<agent_protocol::preview::PreviewRecordingArtifact, String>>,
}

pub struct Browser {
    profile: PathBuf,
    executable: PathBuf,
    state: Arc<Mutex<State>>,
    recordings: Arc<Mutex<HashMap<(String, String), ActiveRecording>>>,
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
        let mut recordings = self.recordings.lock().await;
        if let Some(((_, active_tab), _)) = recordings.iter().next() {
            return Err(format!(
                "recording conflict: tab {tab_id} cannot be recorded while tab {active_tab} is already being recorded"
            ));
        }
        let cancel = CancellationToken::new();
        let started = recording::start(
            endpoint,
            tab_id.to_owned(),
            self.bridge_directory.path().join("recordings"),
            width,
            height,
            cancel.clone(),
            self.stop.clone(),
        )?;
        let started_at = started.started_at.clone();
        recordings.insert(
            key.clone(),
            ActiveRecording {
                cancel,
                task: started.task,
            },
        );
        drop(recordings);
        if let Err(error) = recording::await_startup(started.startup).await {
            if let Some(mut active) = self.recordings.lock().await.remove(&key) {
                active.cancel.cancel();
                if tokio::time::timeout(std::time::Duration::from_secs(5), &mut active.task)
                    .await
                    .is_err()
                {
                    active.task.abort();
                }
            }
            return Err(format!("recording failed for tab {tab_id}: {error}"));
        }
        Ok(agent_protocol::preview::PreviewRecordingStatus {
            tab_id: tab_id.to_owned(),
            recording: true,
            started_at: Some(started_at),
        })
    }

    pub async fn stop_preview_recording(
        &self,
        thread: &str,
        tab_id: &str,
    ) -> Result<agent_protocol::preview::PreviewRecordingArtifact, String> {
        let key = (thread.to_owned(), tab_id.to_owned());
        let active = self
            .recordings
            .lock()
            .await
            .remove(&key)
            .ok_or_else(|| format!("recording is not active for preview tab {tab_id}"))?;
        active.cancel.cancel();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(
                agent_protocol::preview::PREVIEW_RECORDING_MAX_DURATION_SECONDS,
            ),
            active.task,
        )
        .await
        .map_err(|_| format!("recording stop timeout for tab {tab_id} after 120000ms"))?
        .map_err(|error| format!("recording cleanup failed for tab {tab_id}: {error}"))?;
        result
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
    fn socket(&self) -> PathBuf {
        self.bridge_directory.path().join("bridge.sock")
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let active = std::mem::take(&mut *self.recordings.lock().await);
        for (_, mut recording) in active {
            recording.cancel.cancel();
            if tokio::time::timeout(std::time::Duration::from_secs(10), &mut recording.task)
                .await
                .is_err()
            {
                recording.task.abort();
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
        if frame.image_id == request.image_id {
            frame.image.clear();
        }
        drop(state);
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
        let _ = self.stop_preview_recording(thread, tab_id).await;
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        {
            let page = state.pages.get(thread).unwrap();
            if !page.preview_tabs.contains(tab_id) {
                return Err("preview tab was not found".into());
            }
        }
        let chrome = state.chrome.as_mut().unwrap();
        chrome.close_target(tab_id).await?;
        let page = state.pages.get_mut(thread).unwrap();
        page.tabs.retain(|id| id != tab_id);
        page.viewports.remove(tab_id);
        page.preview_tabs.remove(tab_id);
        page.preview_settings.remove(tab_id);
        if page.active == tab_id {
            page.active = page.tabs.last().cloned().unwrap_or_default();
        }
        drop(state);
        if let Some(preview) = self.preview.get()
            && let Ok(thread_id) = agent_domain::ThreadId::new(thread.to_owned())
        {
            preview.close(&thread_id, Some(tab_id));
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
        let browser_result = {
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
                local_servers: Vec::new(),
                scanned_at: String::new(),
                server_epoch: String::new(),
                revision: 0,
                scanner_epoch: String::new(),
                scanner_revision: 0,
            }
        };
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
