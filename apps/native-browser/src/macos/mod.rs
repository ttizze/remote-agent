//! One native owner retains pages across desktop and phone clients. The authenticated Host routes PC, phone and
//! tool operations to this owner; losing one RPC never destroys its tabs.
mod cookies;
mod delegate;
use native_focus as focus;
mod input;
mod platform;

use agent_local_host::browser::RendererLease;
use agent_protocol::{browser::*, browser_native::*};
use async_channel::Sender;
use async_task::Task;
use objc2::{AnyThread, MainThreadMarker, rc::Retained};
use objc2_app_kit::NSEventType;
use objc2_app_kit::NSView;
use objc2_foundation::NSRect;
use objc2_foundation::{NSCopying, NSString, NSUUID};
use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Duration};
use std::{path::PathBuf, rc::Weak};
use tokio_util::task::TaskTracker;
use wry::{WebViewBuilderExtMacos, WebViewExtMacOS, raw_window_handle::HasWindowHandle};

use crate::saved::{Page, Saved, SavedTab};
struct Tab {
    saved: SavedTab,
    view: Rc<wry::WebView>,
    delegate: Retained<delegate::Delegate>,
    input: Retained<input::InputAck>,
}
impl Drop for Tab {
    fn drop(&mut self) {
        focus::hide(&self.view.webview());
    }
}

pub(super) enum Event {
    Changed,
    Navigation { scope: String, id: String },
}

enum Effect {
    Popup {
        scope: String,
        id: String,
        opener: String,
        raw: wry::WebView,
        delegate: Retained<delegate::Delegate>,
        input: Retained<input::InputAck>,
    },
    Close {
        scope: String,
        id: String,
    },
}
struct Call {
    request: NativeRequest,
    reply: Sender<NativeReply>,
}
pub struct Prepared {
    lease: Arc<RendererLease>,
    listener: tokio::net::UnixListener,
    saved: Saved,
}
struct State {
    parent: Retained<NSView>,
    runtime: tokio::runtime::Handle,
    observers: Vec<Task<()>>,
    lease: Arc<RendererLease>,
    profile: Retained<WKWebViewConfiguration>,
    profile_id: String,
    pages: HashMap<String, Page>,
    tabs: Vec<Tab>,
    images: HashMap<String, CachedImage>,
    effects: Rc<RefCell<Vec<Effect>>>,
    changed: Sender<Event>,
    visible: Option<Visible>,
    stop: tokio_util::sync::CancellationToken,
    save: Sender<Saved>,
    jobs: Vec<(Option<String>, Task<()>)>,
}
struct CachedImage {
    tab: String,
    generation: u64,
    image: Vec<u8>,
    id: String,
}
struct Visible {
    scope: String,
    id: String,
    bounds: NSRect,
    parent: Retained<NSView>,
}
#[derive(Clone)]
struct Ticket {
    scope: String,
    id: String,
    generation: u64,
    width: u32,
    view: Rc<wry::WebView>,
    input: Retained<input::InputAck>,
    delegate: Retained<delegate::Delegate>,
}

fn build(
    parent: &impl HasWindowHandle,
    scope: &str,
    id: &str,
    config: Retained<WKWebViewConfiguration>,
    effects: Rc<RefCell<Vec<Effect>>>,
    changed: Sender<Event>,
) -> Result<
    (
        wry::WebView,
        Retained<delegate::Delegate>,
        Retained<input::InputAck>,
    ),
    String,
> {
    let config = config.copy();
    let input = input::InputAck::install(&config);
    let loaded = changed.clone();
    let raw = wry::WebViewBuilder::new()
        .with_webview_configuration(config)
        .with_visible(false)
        .with_accept_first_mouse(true)
        .with_navigation_handler(|url| browser_url(&url).is_ok())
        .with_on_page_load_handler({
            let scope = scope.to_owned();
            let id = id.to_owned();
            move |event, _| {
                let event = match event {
                    wry::PageLoadEvent::Started => Event::Navigation {
                        scope: scope.clone(),
                        id: id.clone(),
                    },
                    wry::PageLoadEvent::Finished => Event::Changed,
                };
                let _ = loaded.try_send(event);
            }
        })
        .build_as_child(parent)
        .map_err(|e| e.to_string())?;
    // A phone may use a conversation whose native child is hidden on the PC.
    // Keep that page's timers and layout alive using WebKit's public policy.
    unsafe {
        raw.webview()
            .configuration()
            .preferences()
            .setInactiveSchedulingPolicy(objc2_web_kit::WKInactiveSchedulingPolicy::None);
    }
    let delegate = delegate::Delegate::install(&raw, scope.into(), id.into(), effects, changed);
    Ok((raw, delegate, input))
}

/// Prepared off the AppKit thread; the private lease is never replaced while held.
impl Prepared {
    pub async fn load(directory: PathBuf) -> anyhow::Result<Self> {
        let (lease, saved) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let lease = Arc::new(RendererLease::acquire(&directory)?);
            let file = directory.join("native-browser.json");
            let saved = if file.exists() {
                serde_json::from_slice(&std::fs::read(&file)?)?
            } else {
                Saved::default()
            };
            saved.validate().map_err(anyhow::Error::msg)?;
            agent_local_host::save_private_json(&file, &saved)?;
            Ok((lease, saved))
        })
        .await??;
        let listener = tokio::net::UnixListener::bind(&lease.renderer.socket)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &lease.renderer.socket,
            std::fs::Permissions::from_mode(0o600),
        )?;
        Ok(Self {
            lease,
            listener,
            saved,
        })
    }
}

/// AppKit owns every view. This handle and all its methods stay on the main thread.
#[derive(Clone)]
pub struct Browser(Rc<RefCell<State>>);

/// Dropping startup cancels its main-thread waiter before it can install pages.
pub struct Startup {
    _task: Task<()>,
}

impl Browser {
    pub fn start(
        parent: &NSView,
        directory: PathBuf,
        runtime: tokio::runtime::Handle,
        closing: TaskTracker,
        ready: impl FnOnce(Result<Self, String>) + 'static,
    ) -> Startup {
        let parent: Retained<NSView> = parent.into();
        let prepared = runtime.spawn(Prepared::load(directory));
        Startup {
            _task: spawn_local(async move {
                let result = match prepared.await {
                    Ok(Ok(prepared)) => Self::new(prepared, &parent, runtime, &closing),
                    Ok(Err(reason)) => Err(reason.to_string()),
                    Err(reason) => Err(reason.to_string()),
                };
                ready(result);
            }),
        }
    }
    pub fn new(
        prepared: Prepared,
        parent: &NSView,
        runtime: tokio::runtime::Handle,
        closing: &TaskTracker,
    ) -> Result<Self, String> {
        State::new(prepared, parent, runtime, closing).map(Self)
    }
    pub fn shutdown(&self) {
        self.0.borrow_mut().shutdown();
    }
    pub fn renderer_id(&self) -> String {
        self.0.borrow().lease.renderer.id.clone()
    }
    pub fn hide(&self, scope: &str) {
        let mut owner = self.0.borrow_mut();
        if owner.visible.as_ref().is_some_and(|v| v.scope == scope) {
            owner.visible = None;
            owner.visibility();
        }
    }
    /// A retiring native surface cannot hide a newer surface of the same conversation.
    pub fn unmount(&self, parent: &NSView) {
        let mut owner = self.0.borrow_mut();
        if owner
            .visible
            .as_ref()
            .is_some_and(|visible| &*visible.parent == parent)
        {
            owner.visible = None;
            owner.visibility();
        }
    }
    pub fn has_surface(&self, scope: &str, renderer: &str, tab: &str) -> bool {
        let owner = self.0.borrow();
        owner.lease.renderer.id == renderer
            && owner.pages.get(scope).is_some_and(|p| p.active == tab)
            && owner
                .tabs
                .iter()
                .any(|t| t.saved.scope == scope && t.saved.id == tab)
    }
    pub fn has_focus(&self, scope: &str, tab: &str) -> bool {
        self.0
            .borrow()
            .tabs
            .iter()
            .find(|t| t.saved.scope == scope && t.saved.id == tab)
            .is_some_and(|tab| focus::has_focus(&tab.view.webview()))
    }
    /// Bounds use the parent's top-left coordinate system, as Wry's child API does.
    pub fn layout(&self, scope: &str, tab: &str, parent: &NSView, bounds: NSRect) {
        let mut owner = self.0.borrow_mut();
        owner.visible = Some(Visible {
            scope: scope.into(),
            id: tab.into(),
            parent: parent.into(),
            bounds,
        });
        owner.visibility();
    }
}

// async-task enforces the creating thread for local futures. Dispatch always
// runs and drops its Runnable on AppKit's main queue, including cancellation.
fn spawn_local(work: impl Future<Output = ()> + 'static) -> Task<()> {
    assert!(MainThreadMarker::new().is_some());
    let (runnable, task) = async_task::spawn_local(work, |runnable: async_task::Runnable| {
        dispatch2::DispatchQueue::main().exec_async(move || {
            runnable.run();
        });
    });
    runnable.schedule();
    task
}

impl State {
    fn shutdown(&mut self) {
        if self.stop.is_cancelled() {
            return;
        }
        self.refresh();
        self.stop.cancel();
        self.visible = None;
        self.jobs.clear();
        self.observers.clear();
        self.effects.borrow_mut().clear();
        self.tabs.clear();
        self.images.clear();
        self.save.close();
    }
    fn new(
        prepared: Prepared,
        parent: &NSView,
        runtime: tokio::runtime::Handle,
        closing: &TaskTracker,
    ) -> Result<Rc<RefCell<Self>>, String> {
        let Prepared {
            lease,
            listener,
            mut saved,
        } = prepared;
        if parent.window().is_none() {
            return Err("ブラウザのウィンドウが閉じています。".into());
        }
        let mtm = MainThreadMarker::new().unwrap();
        let profile = unsafe { WKWebViewConfiguration::new(mtm) };
        let uuid = NSUUID::initWithUUIDString(NSUUID::alloc(), &NSString::from_str(&saved.profile))
            .ok_or("プロファイルを開けません。")?;
        unsafe {
            profile.setWebsiteDataStore(&WKWebsiteDataStore::dataStoreForIdentifier(&uuid, mtm));
        }
        for page in saved.pages.values_mut() {
            page.generation = page.generation.wrapping_add(1);
        }
        let (changed, events) = async_channel::unbounded();
        let (calls, requests) = async_channel::bounded(32);
        let (save, saves) = async_channel::unbounded();
        let directory = lease.state_directory().to_owned();
        closing.spawn_on(async move {
            while let Ok(mut saved) = saves.recv().await {
                while let Ok(newer) = saves.try_recv() { saved = newer; }
                let file = directory.join("native-browser.json");
                let result = tokio::task::spawn_blocking(move || agent_local_host::save_private_json(&file, &saved)).await;
                if !matches!(result, Ok(Ok(()))) { tracing::error!(target:"agent", operation="browser_save", "Cannot save native browser tabs"); }
            }
        }, &runtime);
        let stop = tokio_util::sync::CancellationToken::new();
        let server_stop = stop.clone();
        closing.spawn_on(
            async move {
                serve(listener, calls, server_stop).await;
            },
            &runtime,
        );
        let Saved {
            profile: profile_id,
            pages,
            tabs: restore,
        } = saved;
        let mut state = Self {
            parent: parent.into(),
            runtime,
            observers: Vec::new(),
            lease,
            profile,
            profile_id,
            pages,
            tabs: Vec::new(),
            images: HashMap::new(),
            effects: Default::default(),
            changed,
            visible: None,
            stop,
            save,
            jobs: Vec::new(),
        };
        {
            for saved in restore {
                browser_url(&saved.url)?;
                let (raw, delegate, input) = build(
                    &parent_handle(parent),
                    &saved.scope,
                    &saved.id,
                    state.profile.clone(),
                    state.effects.clone(),
                    state.changed.clone(),
                )?;
                raw.load_url(&saved.url).map_err(|e| e.to_string())?;
                state.attach(saved, raw, delegate, input);
            }
            state.visibility();
            state.lease.publish().map_err(|e| e.to_string())?;
        }
        let owner = Rc::new(RefCell::new(state));
        let weak = Rc::downgrade(&owner);
        let requests_job = spawn_local(async move {
            while let Ok(call) = requests.recv().await {
                let Some(owner) = weak.upgrade() else {
                    break;
                };
                State::execute(&owner, call);
            }
        });
        let weak = Rc::downgrade(&owner);
        let events_job = spawn_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(owner) = weak.upgrade() else {
                    break;
                };
                let mut owner = owner.borrow_mut();
                if let Event::Navigation { scope, id } = event
                    && owner
                        .tabs
                        .iter()
                        .any(|t| t.saved.scope == scope && t.saved.id == id)
                    && let Some(page) = owner.pages.get_mut(&scope)
                {
                    page.generation = page.generation.wrapping_add(1);
                    owner.persist();
                }
                owner.refresh();
            }
        });
        owner.borrow_mut().observers = vec![requests_job, events_job];
        Ok(owner)
    }
    fn attach(
        &mut self,
        saved: SavedTab,
        raw: wry::WebView,
        delegate: Retained<delegate::Delegate>,
        input: Retained<input::InputAck>,
    ) {
        let view = Rc::new(raw);
        self.tabs.push(Tab {
            saved,
            view,
            delegate,
            input,
        });
    }
    fn persist(&self) {
        let saved = Saved {
            profile: self.profile_id.clone(),
            pages: self.pages.clone(),
            tabs: self.tabs.iter().map(|t| t.saved.clone()).collect(),
        };
        let _ = self.save.try_send(saved);
    }
    fn visibility(&self) {
        for tab in &self.tabs {
            let Some(page) = self.pages.get(&tab.saved.scope) else {
                continue;
            };
            let view = &tab.view;
            if view.webview().window().is_none() {
                focus::hide(&view.webview());
                continue;
            }
            let visible = self.visible.as_ref().filter(|v| {
                v.scope == tab.saved.scope && v.id == tab.saved.id && page.active == tab.saved.id
            });
            if let Some(visible) = visible {
                let web = view.webview();
                if unsafe { web.superview() }.as_deref() != Some(&*visible.parent) {
                    focus::release(&web);
                    visible.parent.addSubview(&web);
                }
                let bounds = visible.bounds;
                let scale = (bounds.size.width / f64::from(page.width))
                    .min(bounds.size.height / f64::from(page.height));
                if scale <= 0. {
                    focus::hide(&view.webview());
                    continue;
                }
                let width = f64::from(page.width) * scale;
                let height = f64::from(page.height) * scale;
                let _ = view.zoom(scale);
                let _ = view.set_bounds(wry::Rect {
                    position: wry::dpi::LogicalPosition::new(
                        bounds.origin.x + (bounds.size.width - width) / 2.,
                        bounds.origin.y + (bounds.size.height - height) / 2.,
                    )
                    .into(),
                    size: wry::dpi::LogicalSize::new(width, height).into(),
                });
                let _ = view.set_visible(true);
            } else {
                let web = view.webview();
                focus::hide(&web);
                if unsafe { web.superview() }.as_deref() != Some(&*self.parent) {
                    self.parent.addSubview(&web);
                }
                let _ = view.zoom(1.);
                let _ = view.set_bounds(wry::Rect {
                    position: wry::dpi::LogicalPosition::new(0., 0.).into(),
                    size: wry::dpi::LogicalSize::new(page.width, page.height).into(),
                });
            }
        }
    }
    fn refresh(&mut self) {
        let effects = std::mem::take(&mut *self.effects.borrow_mut());
        let mut changed = false;
        for effect in effects {
            match effect {
                Effect::Popup {
                    scope,
                    id,
                    opener,
                    raw,
                    delegate,
                    input,
                } => {
                    if self.tabs.iter().filter(|t| t.saved.scope == scope).count() >= 24 {
                        continue;
                    }
                    let page = self.pages.entry(scope.clone()).or_default();
                    page.active = id.clone();
                    page.generation = page.generation.wrapping_add(1);
                    self.attach(
                        SavedTab {
                            scope,
                            id,
                            opener: Some(opener),
                            url: "about:blank".into(),
                        },
                        raw,
                        delegate,
                        input,
                    );
                    changed = true;
                }
                Effect::Close { scope, id } => {
                    let _ = self.close(&scope, &id);
                    changed = true;
                }
            }
        }
        for tab in &mut self.tabs {
            let url = unsafe { tab.view.webview().URL().and_then(|u| u.absoluteString()) }
                .map(|s| s.to_string())
                .unwrap_or_default();
            if !url.is_empty() && url != tab.saved.url {
                tab.saved.url = url;
                if let Some(page) = self.pages.get_mut(&tab.saved.scope) {
                    page.generation = page.generation.wrapping_add(1);
                }
                changed = true;
            }
        }
        self.visibility();
        if changed {
            self.persist();
        }
    }
    fn close(&mut self, scope: &str, id: &str) -> Result<(), String> {
        let at = self
            .tabs
            .iter()
            .position(|t| t.saved.scope == scope && t.saved.id == id)
            .ok_or("この会話のタブではありません。")?;
        self.jobs.retain(|(tab, _)| tab.as_deref() != Some(id));
        let mut tab = self.tabs.remove(at);
        let page = self.pages.get_mut(scope).ok_or("ページがありません。")?;
        if page.active == id {
            page.active = tab
                .saved
                .opener
                .take()
                .and_then(|opener| {
                    self.tabs
                        .iter()
                        .find(|t| t.saved.scope == scope && t.saved.id == opener)
                })
                .or_else(|| self.tabs.iter().rev().find(|t| t.saved.scope == scope))
                .map(|t| t.saved.id.clone())
                .unwrap_or_default();
        }
        page.generation = page.generation.wrapping_add(1);
        if !self.tabs.iter().any(|t| t.saved.scope == scope) {
            self.pages.remove(scope);
            self.images.remove(scope);
        }
        Ok(())
    }
    fn open(&mut self, scope: &str, url: &str) -> Result<(), String> {
        if self.tabs.iter().filter(|t| t.saved.scope == scope).count() >= 24 {
            return Err("タブは会話ごとに24個までです。".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (raw, delegate, input) = build(
            &parent_handle(&self.parent),
            scope,
            &id,
            self.profile.clone(),
            self.effects.clone(),
            self.changed.clone(),
        )?;
        raw.load_url(url).map_err(|e| e.to_string())?;
        let page = self.pages.entry(scope.into()).or_default();
        page.active = id.clone();
        page.generation = page.generation.wrapping_add(1);
        self.attach(
            SavedTab {
                scope: scope.into(),
                id,
                url: url.into(),
                opener: None,
            },
            raw,
            delegate,
            input,
        );
        Ok(())
    }
    fn ticket(&self, scope: &str) -> Option<Ticket> {
        let page = self.pages.get(scope)?;
        let tab = self
            .tabs
            .iter()
            .find(|t| t.saved.scope == scope && t.saved.id == page.active)?;
        Some(Ticket {
            scope: scope.into(),
            id: page.active.clone(),
            generation: page.generation,
            width: page.width,
            view: tab.view.clone(),
            input: tab.input.clone(),
            delegate: tab.delegate.clone(),
        })
    }
    fn check(&self, ticket: &Ticket) -> Result<(), String> {
        let page = self
            .pages
            .get(&ticket.scope)
            .ok_or("ページが閉じています。")?;
        if page.active != ticket.id || page.generation != ticket.generation {
            return Err("ページが変わりました。画面を更新してください。".into());
        }
        Ok(())
    }
    fn frame(&self, scope: &str) -> BrowserFrame {
        let page = self.pages.get(scope).cloned().unwrap_or_default();
        let mut frame = BrowserFrame {
            tab_id: page.active.clone(),
            width: page.width,
            height: page.height,
            generation: page.generation,
            renderer_id: self.lease.renderer.id.clone(),
            ..Default::default()
        };
        for tab in self.tabs.iter().filter(|t| t.saved.scope == scope) {
            let web = tab.view.webview();
            let title = unsafe { web.title() }
                .map(|s| s.to_string())
                .unwrap_or_default();
            frame.tabs.push(BrowserTab {
                id: tab.saved.id.clone(),
                title,
                url: tab.saved.url.clone(),
            });
            if tab.saved.id == page.active {
                frame.can_go_back = unsafe { web.canGoBack() };
                frame.can_go_forward = unsafe { web.canGoForward() };
                frame.loading = unsafe { web.isLoading() };
                frame.dialog = tab.delegate.dialog();
            }
        }
        frame
    }
    fn execute(owner: &Rc<RefCell<Self>>, call: Call) {
        let mut state = owner.borrow_mut();
        state.refresh();
        let request = call.request;
        let ticket = match state.begin(&request) {
            Ok(ticket) => ticket,
            Err(error) => {
                let _ = call.reply.try_send(Err(error));
                return;
            }
        };
        state.jobs.retain(|(_, job)| !job.is_finished());
        let job_tab = ticket.as_ref().map(|t| t.id.clone());
        let weak = Rc::downgrade(owner);
        let runtime = state.runtime.clone();
        let task = spawn_local(async move {
            // Keep the existing deadline without replaying any dispatched input.
            let deadline = {
                let _entered = runtime.enter();
                tokio::time::sleep(Duration::from_secs(30))
            };
            let result = match futures_util::future::select(Box::pin(run(&weak, &request, ticket)), Box::pin(deadline)).await {
                futures_util::future::Either::Left((result, _)) => result,
                futures_util::future::Either::Right(_) => Err("ブラウザの応答待ちを終了しました。操作が完了している可能性があるため、画面を確認してください。".into()),
            };
            let _ = call.reply.try_send(result);
        });
        state.jobs.push((job_tab, task));
    }
    fn begin(&mut self, request: &NativeRequest) -> Result<Option<Ticket>, String> {
        request.validate()?;
        if self.jobs.iter().filter(|(_, j)| !j.is_finished()).count() >= 32 {
            return Err("ブラウザの処理中です。画面を確認してください。".into());
        }
        let scope = &request.scope;
        let page = self.pages.get(scope).cloned().unwrap_or_default();
        if let Some(displayed) = &request.displayed {
            let action = match &request.action {
                NativeAction::Browser(a) => a.clone(),
                _ => BrowserAction::Credentials,
            };
            validate_displayed_page(
                &page.active,
                page.generation,
                &displayed.tab_id,
                displayed.generation,
                &action,
            )?;
        }
        if !self.pages.contains_key(scope)
            && matches!(
                &request.action,
                NativeAction::Browser(BrowserAction::Read | BrowserAction::Snapshot)
            )
        {
            return Ok(None);
        }
        if self.pages.len() >= 32 && !self.pages.contains_key(scope) {
            return Err("ブラウザを利用する会話は32件までです。".into());
        }
        if let NativeAction::Browser(action) = &request.action {
            validate_viewport_point(page.width, page.height, action)?;
            match action {
                BrowserAction::Read | BrowserAction::Snapshot => {}
                BrowserAction::OpenTab { url } => self.open(scope, url)?,
                BrowserAction::Navigate { url } if page.active.is_empty() => {
                    self.open(scope, url)?
                }
                BrowserAction::CloseTab { id } => self.close(scope, id)?,
                BrowserAction::SelectTab { id } => {
                    if !self
                        .tabs
                        .iter()
                        .any(|t| t.saved.scope == *scope && t.saved.id == *id)
                    {
                        return Err("この会話のタブではありません。".into());
                    }
                    let page = self.pages.get_mut(scope).unwrap();
                    page.active = id.clone();
                    page.generation = page.generation.wrapping_add(1);
                }
                BrowserAction::Resize { width, height } => {
                    let page = self.pages.entry(scope.clone()).or_default();
                    page.width = *width;
                    page.height = *height;
                    page.generation = page.generation.wrapping_add(1);
                }
                _ => {
                    let tab = self
                        .tabs
                        .iter()
                        .find(|t| t.saved.scope == *scope && t.saved.id == page.active)
                        .ok_or("先にURLを開いてください。")?;
                    let view = tab.view.clone();
                    match action {
                        BrowserAction::Navigate { url } => {
                            view.load_url(url).map_err(|e| e.to_string())?
                        }
                        BrowserAction::Back => unsafe {
                            let _ = view.webview().goBack();
                        },
                        BrowserAction::Forward => unsafe {
                            let _ = view.webview().goForward();
                        },
                        BrowserAction::Reload => view.reload().map_err(|e| e.to_string())?,
                        BrowserAction::Dialog { accept, text } => {
                            tab.delegate.respond(*accept, text)?
                        }
                        BrowserAction::Credentials
                        | BrowserAction::ImportCookies { .. }
                        | BrowserAction::ImportPasswords { .. }
                        | BrowserAction::FillPassword { .. }
                        | BrowserAction::DeletePasswords { .. } => {
                            return Err("ログイン情報はホスト経由で操作してください。".into());
                        }
                        _ => {}
                    }
                    if matches!(
                        action,
                        BrowserAction::Navigate { .. }
                            | BrowserAction::Back
                            | BrowserAction::Forward
                            | BrowserAction::Reload
                    ) {
                        self.pages.get_mut(scope).unwrap().generation =
                            page.generation.wrapping_add(1);
                    }
                }
            }
        }
        self.visibility();
        if matches!(
            &request.action,
            NativeAction::Browser(
                BrowserAction::OpenTab { .. }
                    | BrowserAction::CloseTab { .. }
                    | BrowserAction::SelectTab { .. }
                    | BrowserAction::Navigate { .. }
                    | BrowserAction::Resize { .. }
                    | BrowserAction::Back
                    | BrowserAction::Forward
                    | BrowserAction::Reload
            )
        ) {
            self.persist();
        }
        Ok(self.ticket(scope))
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn serve(
    listener: tokio::net::UnixListener,
    calls: Sender<Call>,
    stop: tokio_util::sync::CancellationToken,
) {
    use tokio::io::AsyncReadExt;
    let permits = Arc::new(tokio::sync::Semaphore::new(32));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _=stop.cancelled()=>break,
            Some(_)=tasks.join_next(), if !tasks.is_empty()=>{},
            accepted=listener.accept()=>{
                let Ok((mut stream,_))=accepted else {break};
                let Ok(permit)=permits.clone().try_acquire_owned() else {continue};
                let calls=calls.clone();
                tasks.spawn(async move {
                    let _permit=permit;
                    use agent_transport::native_browser::{read,write};
                    let Ok(Ok(request))=tokio::time::timeout(Duration::from_secs(5),read::<_,NativeRequest>(&mut stream)).await else{return};
                    let (reply,response)=async_channel::bounded(1);
                    if calls.send(Call{request,reply}).await.is_err(){return;}
                    // A disconnected caller does not cancel an operation in the UI owner.
                    let mut closed=[0;1];
                    tokio::select! {
                        reply=response.recv()=>{if let Ok(reply)=reply {let _=write(&mut stream,&reply).await;}},
                        _=stream.read(&mut closed)=>{},
                    }
                });
            }
        }
    }
    tasks.abort_all();
}

async fn run(
    owner: &Weak<RefCell<State>>,
    request: &NativeRequest,
    ticket: Option<Ticket>,
) -> NativeReply {
    if let NativeAction::ImportCookies(cookies) = &request.action {
        let store = unsafe {
            state(owner)?
                .borrow()
                .profile
                .websiteDataStore()
                .httpCookieStore()
        };
        cookies::install(&store, cookies.clone()).await?;
    }
    let Some(ticket) = ticket else {
        if matches!(request.action, NativeAction::FillPassword { .. }) {
            return Err("先にログイン先のURLを開いてください。".into());
        }
        return Ok(state(owner)?.borrow().frame(&request.scope));
    };
    let read = |expression: &str, isolated| {
        platform::receive(platform::evaluate(&ticket.view, expression, isolated))
    };
    let mut value = String::new();
    match &request.action {
        NativeAction::ImportCookies(_) => {}
        NativeAction::FillPassword { origin, script } => {
            let current = unsafe { ticket.view.webview().URL().and_then(|u| u.absoluteString()) }
                .map(|u| u.to_string())
                .unwrap_or_default();
            if agent_protocol::browser_import::password_origin(&current).as_deref()
                != Ok(origin.as_str())
            {
                return Err("ログイン先が変わりました。".into());
            }
            read(script, false).await?;
        }
        NativeAction::Browser(action) => match action {
            BrowserAction::Evaluate { expression } => {
                let result = read(expression, false).await?;
                value = serde_json::to_string(&result).map_err(|e| e.to_string())?;
                if value.len() > 128 * 1024 {
                    return Err("結果が大きすぎます。取得する範囲を絞ってください。".into());
                }
            }
            BrowserAction::Click { x, y }
            | BrowserAction::Hover { x, y }
            | BrowserAction::Drag { x, y, .. } => {
                // A fresh native frame makes hit testing ready. This is a read,
                // followed by exactly one dispatch of the requested pointer action.
                platform::receive(platform::snapshot(&ticket.view, ticket.width)).await?;
                state(owner)?.borrow().check(&ticket)?;
                let events = match action {
                    BrowserAction::Click { .. } => vec![
                        (*x, *y, NSEventType::LeftMouseDown),
                        (*x, *y, NSEventType::LeftMouseUp),
                    ],
                    BrowserAction::Hover { .. } => vec![(*x, *y, NSEventType::MouseMoved)],
                    BrowserAction::Drag { to_x, to_y, .. } => vec![
                        (*x, *y, NSEventType::LeftMouseDown),
                        (*to_x, *to_y, NSEventType::LeftMouseDragged),
                        (*to_x, *to_y, NSEventType::LeftMouseUp),
                    ],
                    _ => unreachable!(),
                };
                if let Some(result) = ticket
                    .delegate
                    .until_dialog(platform::pointer(&ticket.view, &ticket.input, &events))
                    .await
                {
                    result?;
                }
            }

            BrowserAction::Type { text } => platform::insert(&ticket.view, text)?,
            BrowserAction::Key { key } => platform::key(&ticket.view, *key)?,
            BrowserAction::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                let expression = format!(
                    "(() => {{ let e=document.elementFromPoint({x},{y}); while(e) {{ const s=getComputedStyle(e); if ((e.scrollHeight>e.clientHeight && /(auto|scroll)/.test(s.overflowY)) || (e.scrollWidth>e.clientWidth && /(auto|scroll)/.test(s.overflowX))) {{ e.scrollBy({delta_x},{delta_y});return; }} e=e.parentElement; }} window.scrollBy({delta_x},{delta_y}); }})()"
                );
                read(&expression, true).await?;
            }
            BrowserAction::ClickElement { selector }
            | BrowserAction::Fill { selector, .. }
            | BrowserAction::SelectOption { selector, .. } => {
                let operation = match action {
                    BrowserAction::Fill { .. } => "fill",
                    BrowserAction::SelectOption { .. } => "select",
                    _ => "click",
                };
                let value = match action {
                    BrowserAction::SelectOption { value, .. } => value.as_str(),
                    _ => "",
                };
                let expression = |operation: &str| {
                    format!(
                        "({})({},{},{})",
                        include_str!("element.generated.js"),
                        serde_json::to_string(selector).unwrap(),
                        serde_json::to_string(operation).unwrap(),
                        serde_json::to_string(value).unwrap()
                    )
                };
                if matches!(action, BrowserAction::SelectOption { .. }) {
                    read(&expression(operation), true).await?;
                } else {
                    let point = read(&expression(operation), true).await?;
                    let (x, y) = (
                        point["x"].as_f64().ok_or("要素の位置を取得できません。")?,
                        point["y"].as_f64().ok_or("要素の位置を取得できません。")?,
                    );
                    platform::receive(platform::snapshot(&ticket.view, ticket.width)).await?;
                    state(owner)?.borrow().check(&ticket)?;
                    platform::pointer(
                        &ticket.view,
                        &ticket.input,
                        &[
                            (x, y, NSEventType::LeftMouseDown),
                            (x, y, NSEventType::LeftMouseUp),
                        ],
                    )
                    .await?;
                    if let BrowserAction::Fill { text, .. } = action {
                        read(&expression("focused"), true).await?;
                        state(owner)?.borrow().check(&ticket)?;
                        platform::key(&ticket.view, BrowserKey::SelectAll)?;
                        platform::insert(&ticket.view, text)?;
                    }
                }
            }
            _ => {}
        },
    }
    // Native input is asynchronous in WebKit. A read acknowledges dispatch;
    // neither a failed acknowledgement nor a caller timeout replays the input.
    if matches!(
        &request.action,
        NativeAction::Browser(
            BrowserAction::Type { .. }
                | BrowserAction::Key { .. }
                | BrowserAction::Click { .. }
                | BrowserAction::Hover { .. }
                | BrowserAction::Drag { .. }
                | BrowserAction::Fill { .. }
                | BrowserAction::ClickElement { .. }
        )
    ) && let Some(result) = ticket.delegate.until_dialog(read("true", true)).await
    {
        result?;
    }
    let mut frame = capture_frame(
        owner,
        &request.scope,
        request.capture,
        &request.renderer_id,
        &request.image_id,
    )
    .await?;
    frame.value = value;
    Ok(frame)
}

async fn capture_frame(
    owner: &Weak<RefCell<State>>,
    scope: &str,
    capture: Capture,
    renderer: &str,
    image_id: &str,
) -> NativeReply {
    // Only observations are retried when a navigation overlaps a capture. The
    // operation above has already been dispatched once and is never repeated.
    for _ in 0..3 {
        let (ticket, mut frame) = {
            let owner = state(owner)?;
            let mut owner = owner.borrow_mut();
            owner.refresh();
            (owner.ticket(scope), owner.frame(scope))
        };
        let Some(ticket) = ticket else {
            return Ok(frame);
        };
        let result = ticket
            .delegate
            .until_dialog(async {
                if matches!(capture, Capture::Snapshot) {
                    frame.page_text = platform::receive(platform::evaluate(
                        &ticket.view,
                        include_str!("snapshot.generated.js"),
                        true,
                    ))
                    .await?
                    .as_str()
                    .ok_or("ページ情報を取得できません。")?
                    .to_owned();
                }
                if matches!(capture, Capture::Snapshot)
                    || (matches!(capture, Capture::Viewer) && renderer != frame.renderer_id)
                {
                    frame.input_kind = serde_json::from_value(
                        platform::receive(platform::evaluate(
                            &ticket.view,
                            agent_protocol::browser::INPUT_KIND_SCRIPT,
                            true,
                        ))
                        .await?,
                    )
                    .map_err(|_| "ページの入力先を取得できません。")?;
                    frame.image =
                        platform::receive(platform::snapshot(&ticket.view, ticket.width)).await?;
                    if frame.image.len() > 4 * 1024 * 1024 {
                        return Err("ブラウザ画像が大きすぎます。".into());
                    }
                    use base64::Engine;
                    frame.image_id = base64::engine::general_purpose::STANDARD
                        .encode(ring::digest::digest(&ring::digest::SHA256, &frame.image));
                }
                Ok::<_, String>(())
            })
            .await
            .unwrap_or(Ok(()));
        let valid = (|| {
            let owner = state(owner)?;
            let mut owner = owner.borrow_mut();
            owner.refresh();
            owner.check(&ticket)?;
            frame.dialog = ticket.delegate.dialog();
            if frame.dialog.is_some() {
                if let Some(cached) = owner
                    .images
                    .get(scope)
                    .filter(|c| c.tab == frame.tab_id && c.generation == frame.generation)
                {
                    frame.image = cached.image.clone();
                    frame.image_id = cached.id.clone();
                }
            } else if !frame.image.is_empty() {
                owner.images.insert(
                    scope.into(),
                    CachedImage {
                        tab: frame.tab_id.clone(),
                        generation: frame.generation,
                        image: frame.image.clone(),
                        id: frame.image_id.clone(),
                    },
                );
            }
            Ok::<_, String>(())
        })();
        if valid.is_err() {
            continue;
        }
        result?;
        if matches!(capture, Capture::Viewer) && image_id == frame.image_id {
            frame.image.clear();
        }
        return Ok(frame);
    }
    if matches!(capture, Capture::Snapshot) {
        return Err("ページが変わりました。画面を更新してください。".into());
    }
    // A dispatched action may open a popup that is still navigating. Return
    // fresh tab metadata without an image instead of reporting an action error.
    // The next viewer read captures that page; the input is never replayed.
    let owner = state(owner)?;
    let mut owner = owner.borrow_mut();
    owner.refresh();
    Ok(owner.frame(scope))
}

fn state(owner: &Weak<RefCell<State>>) -> Result<Rc<RefCell<State>>, String> {
    owner
        .upgrade()
        .ok_or_else(|| "ブラウザが閉じています。".into())
}

fn parent_handle(parent: &NSView) -> wry::raw_window_handle::WindowHandle<'_> {
    use wry::raw_window_handle::{AppKitWindowHandle, WindowHandle};
    let handle = AppKitWindowHandle::new(std::ptr::NonNull::from(parent).cast());
    // SAFETY: the borrowed handle is bounded by the retained main-thread NSView.
    unsafe { WindowHandle::borrow_raw(handle.into()) }
}
