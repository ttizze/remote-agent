//! Shared browser authorization and platform renderers.
use super::{ActionOutput, cdp, credentials, mcp};

use agent_protocol::browser::{
    BrowserAction, BrowserFrame, BrowserRequest, BrowserTab, HEIGHT, WIDTH,
};
use base64::Engine;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

struct Page {
    tabs: Vec<String>,
    active: String,
    width: u32,
    height: u32,
    generation: u64,
}
impl Page {
    fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: String::new(),
            width: WIDTH,
            height: HEIGHT,
            generation: 0,
        }
    }
}

#[derive(Default)]
struct State {
    chrome: Option<cdp::Chrome>,
    pages: HashMap<String, Page>,
}

use agent_protocol::browser_native::Capture;

pub struct Browser {
    profile: PathBuf,
    executable: PathBuf,
    state: Mutex<State>,
    pub(super) stop: tokio_util::sync::CancellationToken,
    pub(super) bridge_directory: tempfile::TempDir,
}

impl Browser {
    pub async fn start(profile: PathBuf) -> Result<Arc<Self>, String> {
        let executable = std::env::var_os("AGENT_BROWSER_EXECUTABLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("chromium"));
        let bridge_directory = tempfile::Builder::new()
            .prefix("agent-browser-")
            // macOS Unix sockets have a 104-byte path limit. Nix's TMPDIR may
            // be nested under a long checkout path; keep the socket root short.
            .tempdir_in("/tmp")
            .map_err(|e| e.to_string())?;
        agent_local_host::create_state_directory(bridge_directory.path())
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
        if state.chrome.as_ref().is_some_and(cdp::Chrome::disconnected) {
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
            state.pages.insert(thread.into(), Page::new());
        }
        let page = state.pages.get_mut(thread).unwrap();
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
        if !page.tabs.contains(&page.active) {
            page.active = page.tabs.last().cloned().unwrap_or_default();
        }
        Ok(())
    }

    pub async fn request(&self, request: &BrowserRequest) -> Result<BrowserFrame, String> {
        request.validate()?;
        let thread = request.thread_id.to_string();
        let prepared = if credentials::manages_credentials(&request.action) {
            let origin = {
                let mut state = self.state.lock().await;
                self.ensure(&mut state, &thread).await?;
                validate_request(&state, &thread, request)?;
                let active = state.pages[&thread].active.clone();
                let url = state
                    .chrome
                    .as_mut()
                    .unwrap()
                    .targets()
                    .await?
                    .into_iter()
                    .find(|target| target.target_id == active)
                    .map(|target| target.url)
                    .unwrap_or_default();
                let origin = agent_protocol::browser_import::password_origin(&url).ok();
                if let BrowserAction::FillPassword {
                    origin: expected, ..
                }
                | BrowserAction::DeletePasswords { origin: expected } = &request.action
                {
                    validate_password_origin(origin.as_deref(), expected)?;
                }
                origin
            };
            Some(credentials::prepare(request.action.clone(), origin).await?)
        } else {
            None
        };
        let mut state = self.state.lock().await;
        self.ensure(&mut state, &thread).await?;
        validate_request(&state, &thread, request)?;
        let value = if let Some(prepared) = prepared {
            Self::credential_action(&mut state, &thread, prepared).await?
        } else {
            Self::action(&mut state, &thread, &request.action).await?
        };
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
        super::validate_agent_action(&action)?;
        let mut state = self.state.lock().await;
        self.ensure(&mut state, thread).await?;
        let value = Self::action(&mut state, thread, &action).await?;
        Self::frame(
            &mut state,
            thread,
            if matches!(action, BrowserAction::Snapshot) {
                Capture::Snapshot
            } else {
                Capture::None
            },
            value,
        )
        .await
    }

    async fn credential_action(
        state: &mut State,
        thread: &str,
        prepared: credentials::Prepared,
    ) -> Result<ActionOutput, String> {
        #[cfg(not(test))]
        let _ = (state, thread);
        let mut output = ActionOutput::default();
        match prepared {
            credentials::Prepared::Metadata(metadata) => output.credentials = Some(metadata),
            #[cfg(test)]
            credentials::Prepared::Cookies { cookies, skipped } => {
                output.notice = Some(agent_protocol::browser::BrowserNotice::CookiesImported {
                    count: cookies.len() as u64,
                    skipped: skipped as u64,
                });
                if cookies.is_empty() {
                    return Ok(output);
                }
                let page = &state.pages[thread];
                let chrome = state.chrome.as_mut().unwrap();
                chrome.install_cookies(&cookies).await?;
                let session = chrome.attach(&page.active).await?;
                chrome.action(&session, &BrowserAction::Reload).await?;
                state.pages.get_mut(thread).unwrap().generation += 1;
            }
        }
        Ok(output)
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
        if matches!(action, BrowserAction::Read) {
            return Ok(ActionOutput::default());
        }
        let chrome = state.chrome.as_mut().unwrap();
        match action {
            BrowserAction::OpenTab { url } => {
                if page.tabs.len() >= 24 {
                    return Err("タブは24個までです。".into());
                }
                let id = chrome.create().await?;
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

                return Ok(ActionOutput::default());
            }
            _ => (),
        }
        if page.active.is_empty() && matches!(action, BrowserAction::Navigate { .. }) {
            let id = chrome.create().await?;
            page.tabs.push(id.clone());
            page.active = id;
        }
        if page.active.is_empty() {
            return match action {
                BrowserAction::Read | BrowserAction::Snapshot => Ok(ActionOutput::default()),
                BrowserAction::Resize { width, height } => {
                    page.width = *width;
                    page.height = *height;
                    page.generation += 1;
                    Ok(ActionOutput::default())
                }
                _ => Err("先にURLを開いてください。".into()),
            };
        }
        let session = chrome.attach(&page.active).await?;
        if let BrowserAction::Resize { width, height } = action {
            chrome.resize(&session, *width, *height).await?;
            page.width = *width;
            page.height = *height;
            page.generation += 1;
            return Ok(ActionOutput::default());
        }
        agent_protocol::browser::validate_viewport_point(page.width, page.height, action)?;
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
        if page.active.is_empty() {
            return Ok(BrowserFrame {
                width: page.width,
                height: page.height,
                generation: page.generation,
                ..Default::default()
            });
        }
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
        let image_id = if image.is_empty() {
            String::new()
        } else {
            base64::engine::general_purpose::STANDARD
                .encode(ring::digest::digest(&ring::digest::SHA256, &image))
        };
        let targets = chrome.targets().await?;
        let (can_go_back, can_go_forward) = chrome.history(&session).await?;
        let input_kind = if !matches!(capture, Capture::None) && chrome.dialog(&session).is_none() {
            serde_json::from_value(
                chrome
                    .evaluate(&session, agent_protocol::browser::INPUT_KIND_SCRIPT)
                    .await?,
            )
            .map_err(|_| "ページの入力先を取得できません。")?
        } else {
            None
        };
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
            input_kind,
            generation: page.generation + chrome.navigation(&page.active),
            can_go_back,
            can_go_forward,
            loading: chrome.loading(&session),
            page_text: output.page_text,
            value: output.value,
            notice: output.notice,
            credentials: output.credentials,
            renderer_id: String::new(),
        })
    }
}

fn validate_password_origin(current: Option<&str>, requested: &str) -> Result<(), String> {
    if current == Some(requested) {
        Ok(())
    } else {
        Err("表示中のログイン先が変わりました。もう一度一覧を開いてください。".into())
    }
}

fn validate_request(state: &State, thread: &str, request: &BrowserRequest) -> Result<(), String> {
    let page = &state.pages[thread];
    let generation = page.generation + state.chrome.as_ref().unwrap().navigation(&page.active);
    agent_protocol::browser::validate_displayed_page(
        &page.active,
        generation,
        &request.tab_id,
        request.generation,
        &request.action,
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
