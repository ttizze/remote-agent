//! Owns the persistent BEX profile and conversation-scoped shared pages.
mod cdp;
pub mod mcp;

use agent_protocol::browser::{
    BrowserAction, BrowserFrame, BrowserRequest, BrowserTab, HEIGHT, WIDTH,
};
use base64::Engine;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

#[derive(Default)]
struct Page {
    tabs: Vec<String>,
    active: String,
}

#[derive(Default)]
struct State {
    chrome: Option<cdp::Chrome>,
    pages: HashMap<String, Page>,
    aliases: HashMap<String, String>,
}

pub struct Browser {
    profile: PathBuf,
    executable: PathBuf,
    state: Mutex<State>,
    stop: tokio_util::sync::CancellationToken,
    bridge_directory: tempfile::TempDir,
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
            for page in state.pages.values_mut() {
                page.tabs.clear();
                page.active.clear();
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
        if page.tabs.is_empty() {
            let id = chrome.create().await?;
            page.tabs.push(id.clone());
            page.active = id;
        } else if !page.tabs.contains(&page.active) {
            page.active = page.tabs.last().unwrap().clone();
        }
        Ok(())
    }

    pub async fn request(&self, request: &BrowserRequest) -> Result<BrowserFrame, String> {
        request.validate()?;
        let thread = request.thread_id.to_string();
        let mut state = self.state.lock().await;
        self.ensure(&mut state, &thread).await?;
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
        Self::action(&mut state, &thread, &action).await?;
        Self::frame(&mut state, &thread).await
    }

    async fn action(state: &mut State, thread: &str, action: &BrowserAction) -> Result<(), String> {
        let page = state.pages.get_mut(thread).unwrap();
        if let BrowserAction::SelectTab { id } = action {
            if !page.tabs.contains(id) {
                return Err("この会話のタブではありません。".into());
            }
            page.active = id.clone();
            return Ok(());
        }
        if matches!(action, BrowserAction::Read) {
            return Ok(());
        }
        let chrome = state.chrome.as_mut().unwrap();
        let session = chrome.attach(&page.active).await?;
        chrome.action(&session, action).await
    }

    async fn frame(state: &mut State, thread: &str) -> Result<BrowserFrame, String> {
        let page = &state.pages[thread];
        let chrome = state.chrome.as_mut().unwrap();
        let session = chrome.attach(&page.active).await?;
        let image = chrome.screenshot(&session).await?;
        let image_id = base64::engine::general_purpose::STANDARD
            .encode(ring::digest::digest(&ring::digest::SHA256, &image));
        let targets = chrome.targets().await?;
        Ok(BrowserFrame {
            tabs: targets
                .into_iter()
                .filter(|target| page.tabs.contains(&target.target_id))
                .map(|target| BrowserTab {
                    id: target.target_id,
                    title: target.title,
                    url: target.url,
                })
                .collect(),
            tab_id: page.active.clone(),
            width: WIDTH,
            height: HEIGHT,
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
