//! Owns the persistent BEX profile and conversation-scoped shared pages.
mod cdp;
pub mod mcp;

use agent_protocol::browser::{
    BrowserAction, BrowserControl, BrowserFrame, BrowserRequest, BrowserTab, HEIGHT, WIDTH,
};
use base64::Engine;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

struct Page {
    tabs: Vec<String>,
    active: String,
    owner: Option<String>,
    waiting: bool,
    token: String,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: String::new(),
            owner: None,
            waiting: false,
            token: Uuid::new_v4().to_string(),
        }
    }
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
    changed: watch::Sender<u64>,
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
            changed: watch::channel(0).0,
            stop: Default::default(),
        });
        mcp::listen(&browser)?;
        Ok(browser)
    }
    // Codex assigns the thread ID after accepting its MCP configuration.
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
    fn notify(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }
    pub async fn revoke_device(&self, principal: &str) {
        let mut state = self.state.lock().await;
        for page in state
            .pages
            .values_mut()
            .filter(|page| page.owner.as_deref() == Some(principal))
        {
            page.owner = None;
            page.waiting = true;
            page.token = Uuid::new_v4().to_string();
        }
        self.notify();
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        if let Some(chrome) = self.state.lock().await.chrome.take() {
            chrome.shutdown().await;
        }
    }

    async fn ensure(&self, state: &mut State, thread: &str) -> Result<(), String> {
        agent_protocol::session::SessionRef::from_thread_id(thread).map_err(str::to_owned)?;
        if state.chrome.as_ref().is_some_and(|chrome| chrome.broken) {
            if let Some(chrome) = state.chrome.take() {
                chrome.shutdown().await;
            }
            for page in state.pages.values_mut() {
                page.tabs.clear();
                page.active.clear();
                page.token = Uuid::new_v4().to_string();
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

    pub async fn request(
        &self,
        principal: &str,
        request: &BrowserRequest,
    ) -> Result<BrowserFrame, String> {
        request.action.validate()?;
        let mut state = self.state.lock().await;
        self.ensure(&mut state, &request.thread_id).await?;
        let page = state.pages.get_mut(&request.thread_id).unwrap();
        authorize(
            page,
            principal,
            &request.control_token,
            &request.tab_id,
            &request.action,
        )?;
        match &request.action {
            BrowserAction::TakeControl => {
                page.owner = Some(principal.into());
                page.token = Uuid::new_v4().to_string();
                self.notify();
            }
            BrowserAction::ReleaseControl => {
                page.owner = None;
                page.waiting = false;
                page.token = Uuid::new_v4().to_string();
                self.notify();
            }
            _ => {
                self.action(&mut state, &request.thread_id, &request.action)
                    .await?
            }
        }
        let mut frame = self
            .frame(&mut state, &request.thread_id, principal)
            .await?;
        if frame.image_id == request.image_id {
            frame.image.clear();
        }
        Ok(frame)
    }

    pub(super) async fn agent(
        &self,
        thread: &str,
        action: BrowserAction,
        wait_for_user: bool,
    ) -> Result<BrowserFrame, String> {
        action.validate()?;
        let thread = self
            .state
            .lock()
            .await
            .aliases
            .get(thread)
            .cloned()
            .unwrap_or_else(|| thread.to_owned());
        let thread = thread.as_str();
        let mut changed = self.changed.subscribe();
        let mut waited = false;
        if wait_for_user {
            let mut state = self.state.lock().await;
            self.ensure(&mut state, thread).await?;
            state.pages.get_mut(thread).unwrap().waiting = true;
            self.notify();
        }
        loop {
            let mut state = self.state.lock().await;
            self.ensure(&mut state, thread).await?;
            let page = &state.pages[thread];
            if page.owner.is_none() && !page.waiting {
                if waited && !matches!(action, BrowserAction::Read) {
                    return Err("ユーザーの操作が完了しました。待機していた操作は実行していません。screenshot で現在の画面を確認してから続けてください。".into());
                }
                self.action(&mut state, thread, &action).await?;
                return self.frame(&mut state, thread, "agent").await;
            }
            waited = true;
            drop(state);
            tokio::select! {
                _ = self.stop.cancelled() => return Err("Hostを終了しました。".into()),
                result = tokio::time::timeout(std::time::Duration::from_secs(1800), changed.changed()) => {
                    if !matches!(result, Ok(Ok(()))) { return Err("ユーザーがブラウザを操作中です。「AIに戻す」を待ってください。".into()); }
                }
            }
        }
    }

    async fn action(
        &self,
        state: &mut State,
        thread: &str,
        action: &BrowserAction,
    ) -> Result<(), String> {
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

    async fn frame(
        &self,
        state: &mut State,
        thread: &str,
        principal: &str,
    ) -> Result<BrowserFrame, String> {
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
            control_token: page.token.clone(),
            control: match page.owner.as_deref() {
                Some(owner) if owner == principal => BrowserControl::Yours,
                Some(_) => BrowserControl::Other,
                None if page.waiting => BrowserControl::AwaitingHuman,
                None => BrowserControl::Agent,
            },
            width: WIDTH,
            height: HEIGHT,
            image,
            image_id,
            dialog: chrome.dialog(&session),
        })
    }
}

fn authorize(
    page: &Page,
    principal: &str,
    token: &str,
    tab: &str,
    action: &BrowserAction,
) -> Result<(), String> {
    if matches!(action, BrowserAction::Read) {
        return Ok(());
    }
    if page.token != token {
        return Err("操作権が変わりました。最新の画面で再試行してください。".into());
    }
    if matches!(action, BrowserAction::TakeControl) {
        if page.owner.as_deref().is_none_or(|owner| owner == principal) {
            return Ok(());
        }
        return Err("別の端末が操作中です。".into());
    }
    if page.owner.as_deref() != Some(principal) {
        return Err("「自分で操作する」を押してから操作してください。".into());
    }
    if !matches!(
        action,
        BrowserAction::ReleaseControl | BrowserAction::SelectTab { .. }
    ) && page.active != tab
    {
        return Err("表示中のタブが変わりました。最新の画面で再試行してください。".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
