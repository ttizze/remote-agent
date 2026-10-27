//! Owns isolated conversation contexts, shared tabs, and human/agent control.
mod cdp;
pub mod mcp;

use agent_protocol::browser::{
    BrowserAction, BrowserController, BrowserFrame, BrowserRequest, BrowserTab, HEIGHT, WIDTH,
};
use base64::Engine;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

struct Page {
    context: String,
    tabs: Vec<String>,
    active: String,
    width: u32,
    height: u32,
    controller: String,
    generation: u64,
    lease: Instant,
}
impl Page {
    fn new(context: String) -> Self {
        Self {
            context,
            tabs: Vec::new(),
            active: String::new(),
            width: WIDTH,
            height: HEIGHT,
            controller: String::new(),
            generation: 0,
            lease: Instant::now(),
        }
    }
    fn expire_control(&mut self, now: Instant) -> bool {
        if !self.controller.is_empty() && now >= self.lease {
            self.controller.clear();
            self.generation += 1;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
struct State {
    chrome: Option<cdp::Chrome>,
    pages: HashMap<String, Page>,
    aliases: HashMap<String, String>,
}

enum Capture {
    None,
    Viewer,
    Snapshot,
}

#[derive(Default)]
struct ActionOutput {
    page_text: String,
    value: String,
}

pub struct Browser {
    profile: PathBuf,
    executable: PathBuf,
    state: Mutex<State>,
    stop: tokio_util::sync::CancellationToken,
    #[cfg(unix)]
    bridge_directory: tempfile::TempDir,
    #[cfg(windows)]
    bridge_pipe: PathBuf,
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
                    default_executable()
                }
            });
        #[cfg(unix)]
        let bridge_directory = tempfile::Builder::new()
            .prefix("bex-browser-")
            // macOS Unix sockets have a 104-byte path limit. Nix's TMPDIR may
            // be nested under a long checkout path; keep the socket root short.
            .tempdir_in("/tmp")
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        crate::platform::create_state_directory(bridge_directory.path())
            .map_err(|e| e.to_string())?;
        let browser = Arc::new(Self {
            profile,
            executable,
            #[cfg(unix)]
            bridge_directory,
            #[cfg(windows)]
            bridge_pipe: PathBuf::from(format!(r"\\.\pipe\bex-browser-{}", uuid::Uuid::new_v4())),
            state: Mutex::new(State::default()),
            stop: Default::default(),
        });
        mcp::listen(&browser)?;
        Ok(browser)
    }
    // A provider can assign the session ID after accepting its MCP configuration.
    // Bind that startup scope before the first turn can run. Resume uses the ID directly.
    pub async fn bind_scope(&self, scope: String, thread: String) {
        self.state.lock().await.aliases.insert(scope, thread);
    }

    pub fn provider_config(&self, thread: &str) -> Result<serde_json::Value, String> {
        Ok(
            serde_json::json!({"command":std::env::current_exe().map_err(|e| e.to_string())?,
            "args":["browser-mcp", "--socket", self.socket(), "--thread", thread]}),
        )
    }
    fn socket(&self) -> PathBuf {
        #[cfg(windows)]
        return self.bridge_pipe.clone();
        #[cfg(unix)]
        self.bridge_directory.path().join("bridge.sock")
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
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
            state.pages.clear();
        }
        if state.chrome.is_none() {
            state.chrome = Some(cdp::Chrome::launch(&self.profile, &self.executable).await?);
        }
        if !state.pages.contains_key(thread) && state.pages.len() >= 32 {
            return Err("ブラウザを開いている会話が上限に達しました。".into());
        }
        let chrome = state.chrome.as_mut().unwrap();
        if !state.pages.contains_key(thread) {
            let context = chrome.create_context().await?;
            state.pages.insert(thread.into(), Page::new(context));
        }
        let page = state.pages.get_mut(thread).unwrap();
        if page.expire_control(Instant::now()) {
            chrome.invalidate_refs(&page.tabs);
        }
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
        if page.tabs.is_empty() {
            let id = chrome.create(&page.context).await?;
            page.tabs.push(id.clone());
            page.active = id;
        } else if !page.tabs.contains(&page.active) {
            page.active = page.tabs.last().unwrap().clone();
        }
        Ok(())
    }

    pub async fn request(
        &self,
        session: u64,
        request: &BrowserRequest,
    ) -> Result<BrowserFrame, String> {
        request.validate()?;
        let thread = request.thread_id.to_string();
        let client = format!("{session}:{}", request.client_id);
        let mut state = self.state.lock().await;
        self.ensure(&mut state, &thread).await?;
        validate_tab(
            &state.pages[&thread].active,
            &request.tab_id,
            &request.action,
        )?;
        let current_generation = state.pages[&thread].generation
            + state
                .chrome
                .as_ref()
                .unwrap()
                .navigation(&state.pages[&thread].active);
        let page = state.pages.get_mut(&thread).unwrap();
        validate_control(
            &page.controller,
            current_generation,
            &client,
            request.generation,
            &request.action,
        )?;
        match request.action {
            BrowserAction::TakeControl => {
                page.controller = client.clone();
                page.generation += 1;
            }
            BrowserAction::ReleaseControl => {
                page.controller.clear();
                page.generation += 1;
            }
            _ => (),
        }
        if page.controller == client {
            page.lease = Instant::now() + Duration::from_secs(15);
        }
        if matches!(
            request.action,
            BrowserAction::TakeControl | BrowserAction::ReleaseControl
        ) {
            let State { chrome, pages, .. } = &mut *state;
            chrome
                .as_mut()
                .unwrap()
                .invalidate_refs(&pages[&thread].tabs);
        }
        let value = Self::action(&mut state, &thread, &request.action).await?;
        let mut frame = Self::frame(
            &mut state,
            &thread,
            if matches!(request.action, BrowserAction::Snapshot) {
                Capture::Snapshot
            } else {
                Capture::Viewer
            },
            value,
        )
        .await?;
        if state.pages[&thread].controller == client {
            frame.controller = BrowserController::You;
        }
        if frame.image_id == request.image_id {
            frame.image.clear();
        }
        Ok(frame)
    }

    pub(super) async fn agent(
        &self,
        thread: &str,
        action: BrowserAction,
    ) -> Result<BrowserFrame, String> {
        action.validate()?;
        let mut state = self.state.lock().await;
        let thread = state
            .aliases
            .get(thread)
            .cloned()
            .unwrap_or_else(|| thread.to_owned());
        self.ensure(&mut state, &thread).await?;
        let page = &state.pages[&thread];
        validate_control(
            &page.controller,
            page.generation,
            "",
            page.generation,
            &action,
        )?;
        if matches!(
            action,
            BrowserAction::TakeControl | BrowserAction::ReleaseControl
        ) {
            return Err("Only the user can take or release browser control.".into());
        }
        let value = Self::action(&mut state, &thread, &action).await?;
        Self::frame(
            &mut state,
            &thread,
            if matches!(action, BrowserAction::Snapshot) {
                Capture::Snapshot
            } else {
                Capture::None
            },
            value,
        )
        .await
    }

    async fn action(
        state: &mut State,
        thread: &str,
        action: &BrowserAction,
    ) -> Result<ActionOutput, String> {
        let page = state.pages.get_mut(thread).unwrap();
        if let BrowserAction::SelectTab { id } = action {
            if !page.tabs.contains(id) {
                return Err("この会話のタブではありません。".into());
            }
            page.active = id.clone();
            return Ok(ActionOutput::default());
        }
        if matches!(
            action,
            BrowserAction::Read | BrowserAction::TakeControl | BrowserAction::ReleaseControl
        ) {
            return Ok(ActionOutput::default());
        }
        let chrome = state.chrome.as_mut().unwrap();
        match action {
            BrowserAction::OpenTab { url } => {
                if page.tabs.len() >= 24 {
                    return Err("タブは24個までです。".into());
                }
                let id = chrome.create(&page.context).await?;
                page.tabs.push(id.clone());
                page.active = id;
                let session = chrome.attach(&page.active).await?;
                chrome.resize(&session, page.width, page.height).await?;
                return chrome
                    .action(&session, &BrowserAction::Navigate { url: url.clone() })
                    .await;
            }
            BrowserAction::CloseTab { id } => {
                if !page.tabs.contains(id) {
                    return Err("この会話のタブではありません。".into());
                }
                chrome.close_tab(id).await?;
                page.tabs.retain(|tab| tab != id);
                if page.active == *id {
                    page.active = page.tabs.last().cloned().unwrap_or_default();
                }
                if page.tabs.is_empty() {
                    let id = chrome.create(&page.context).await?;
                    page.tabs.push(id.clone());
                    page.active = id;
                }
                return Ok(ActionOutput::default());
            }
            _ => (),
        }
        let session = chrome.attach(&page.active).await?;
        if let BrowserAction::Resize { width, height } = action {
            chrome.resize(&session, *width, *height).await?;
            page.width = *width;
            page.height = *height;
            page.generation += 1;
            return Ok(ActionOutput::default());
        }
        validate_point(page.width, page.height, action)?;
        let output = chrome.action(&session, action).await?;
        if matches!(
            action,
            BrowserAction::Navigate { .. }
                | BrowserAction::Back
                | BrowserAction::Forward
                | BrowserAction::Reload
        ) {
            page.generation += 1;
        }
        Ok(output)
    }

    async fn frame(
        state: &mut State,
        thread: &str,
        capture: Capture,
        output: ActionOutput,
    ) -> Result<BrowserFrame, String> {
        let page = &state.pages[thread];
        let chrome = state.chrome.as_mut().unwrap();
        let session = chrome.attach(&page.active).await?;
        chrome.resize(&session, page.width, page.height).await?;
        let image = match capture {
            Capture::None => Vec::new(),
            _ if chrome.dialog(&session).is_some() => {
                chrome.cached_image(&session).unwrap_or_default()
            }
            Capture::Viewer => chrome.latest_image(&session).await?,
            Capture::Snapshot => chrome.screenshot(&session).await?,
        };
        let image_id = base64::engine::general_purpose::STANDARD
            .encode(ring::digest::digest(&ring::digest::SHA256, &image));
        let targets = chrome.targets().await?;
        let (can_go_back, can_go_forward) = chrome.history(&session).await?;
        Ok(BrowserFrame {
            tabs: page
                .tabs
                .iter()
                .filter_map(|id| targets.iter().find(|target| &target.target_id == id))
                .map(|target| BrowserTab {
                    id: target.target_id.clone(),
                    title: target.title.clone(),
                    url: target.url.clone(),
                })
                .collect(),
            tab_id: page.active.clone(),
            width: page.width,
            height: page.height,
            image,
            image_id,
            dialog: chrome.dialog(&session),
            controller: if page.controller.is_empty() {
                BrowserController::Agent
            } else {
                BrowserController::AnotherViewer
            },
            generation: page.generation + chrome.navigation(&page.active),
            can_go_back,
            can_go_forward,
            loading: chrome.loading(&session),
            page_text: output.page_text,
            value: output.value,
        })
    }
}

fn default_executable() -> PathBuf {
    #[cfg(windows)]
    for (root, relative) in [
        ("PROGRAMFILES(X86)", "Microsoft/Edge/Application/msedge.exe"),
        ("PROGRAMFILES", "Microsoft/Edge/Application/msedge.exe"),
        ("PROGRAMFILES", "Google/Chrome/Application/chrome.exe"),
        ("PROGRAMFILES(X86)", "Google/Chrome/Application/chrome.exe"),
        ("LOCALAPPDATA", "Google/Chrome/Application/chrome.exe"),
    ] {
        if let Some(root) = std::env::var_os(root) {
            let candidate = PathBuf::from(root).join(relative);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("chromium")
}

fn validate_tab(active: &str, displayed: &str, action: &BrowserAction) -> Result<(), String> {
    if !matches!(
        action,
        BrowserAction::Read
            | BrowserAction::Snapshot
            | BrowserAction::SelectTab { .. }
            | BrowserAction::TakeControl
            | BrowserAction::ReleaseControl
    ) && active != displayed
    {
        return Err("表示中のタブが変わりました。最新の画面で再試行してください。".into());
    }
    Ok(())
}

fn validate_control(
    controller: &str,
    current_generation: u64,
    client: &str,
    generation: u64,
    action: &BrowserAction,
) -> Result<(), String> {
    if matches!(
        action,
        BrowserAction::Read | BrowserAction::Snapshot | BrowserAction::TakeControl
    ) {
        return Ok(());
    }
    if controller != client || (!client.is_empty() && generation != current_generation) {
        return Err(if client.is_empty() { "The user is controlling the browser. You may inspect it; wait until the user releases control before acting." } else { "操作権を取得してから操作してください。" }.into());
    }
    Ok(())
}

fn validate_point(width: u32, height: u32, action: &BrowserAction) -> Result<(), String> {
    let point = |x: f64, y: f64| {
        (0.0..f64::from(width)).contains(&x) && (0.0..f64::from(height)).contains(&y)
    };
    let valid = match action {
        BrowserAction::Click { x, y }
        | BrowserAction::Hover { x, y }
        | BrowserAction::Scroll { x, y, .. } => point(*x, *y),
        BrowserAction::Drag { x, y, to_x, to_y } => point(*x, *y) && point(*to_x, *to_y),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err("表示範囲外の座標です。".into())
    }
}

#[cfg(test)]
mod tests;
