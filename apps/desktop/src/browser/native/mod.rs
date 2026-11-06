//! The desktop owns native pages. The authenticated Host routes PC, phone and
//! tool operations to this owner; losing one RPC never destroys its tabs.
mod cookies;
mod delegate;
use native_focus as focus;
mod input;
mod platform;

use agent_local_host::browser::RendererLease;
use agent_protocol::{browser::*, browser_native::*};
use async_channel::Sender;
use gpui_kit::*;
use objc2::{AnyThread, MainThreadMarker, rc::Retained};
use objc2_app_kit::NSEventType;
use objc2_foundation::{NSCopying, NSString, NSUUID};
use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc, time::Duration};
use wry::{WebViewBuilderExtMacos, WebViewExtMacOS, raw_window_handle::HasWindowHandle};

#[derive(Clone, Serialize, Deserialize)]
struct SavedTab {
    scope: String,
    id: String,
    url: String,
    opener: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Page {
    active: String,
    width: u32,
    height: u32,
    generation: u64,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            active: String::new(),
            width: WIDTH,
            height: HEIGHT,
            generation: 0,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    profile: String,
    pages: HashMap<String, Page>,
    tabs: Vec<SavedTab>,
}
impl Default for Saved {
    fn default() -> Self {
        Self {
            profile: uuid::Uuid::new_v4().to_string(),
            pages: HashMap::new(),
            tabs: Vec::new(),
        }
    }
}
impl Saved {
    fn validate(&self) -> Result<(), String> {
        use std::collections::HashSet;
        let profile = uuid::Uuid::parse_str(&self.profile)
            .map_err(|_| "ブラウザのプロファイルが不正です。")?;
        if profile.is_nil() || self.pages.len() > 32 || self.tabs.len() > 32 * 24 {
            return Err("ブラウザ保存データが範囲外です。".into());
        }
        let mut ids = HashSet::new();
        for tab in &self.tabs {
            if tab.id.is_empty()
                || tab.id.len() > 256
                || !ids.insert(&tab.id)
                || !self.pages.contains_key(&tab.scope)
            {
                return Err("保存したタブの所属が不正です。".into());
            }
            browser_url(&tab.url)?;
        }
        for (scope, page) in &self.pages {
            BrowserAction::Resize {
                width: page.width,
                height: page.height,
            }
            .validate()?;
            let tabs: Vec<_> = self.tabs.iter().filter(|t| &t.scope == scope).collect();
            if scope.is_empty()
                || scope.len() > 8192
                || tabs.len() > 24
                || (!page.active.is_empty() && !tabs.iter().any(|t| t.id == page.active))
                || (page.active.is_empty() && !tabs.is_empty())
            {
                return Err("保存した会話のブラウザが不正です。".into());
            }
        }
        Ok(())
    }
}
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
struct Prepared {
    lease: Arc<RendererLease>,
    listener: tokio::net::UnixListener,
    saved: Saved,
}
struct NativeGlobal(Option<Entity<NativeBrowser>>);
impl Global for NativeGlobal {}
struct NativeBrowser {
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
    bounds: Bounds<Pixels>,
}
#[derive(Clone)]
struct Ticket {
    scope: String,
    id: String,
    generation: u64,
    width: u32,
    view: Rc<wry::WebView>,
    input: Retained<input::InputAck>,
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

pub fn init(window: &mut Window, cx: &mut App) {
    cx.set_global(NativeGlobal(None));
    let runtime = cx.global::<crate::Runtime>().clone();
    window.spawn(cx, async move |cx| {
        let prepared = runtime.handle.spawn(async {
            let host = crate::platform::discover_local_host().await?;
            let directory = host.directory;
            let (lease, saved) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let lease = Arc::new(RendererLease::acquire(&directory)?);
                let file = directory.join("native-browser.json");
                let saved = if file.exists() { serde_json::from_slice(&std::fs::read(&file)?)? } else { Saved::default() };
                agent_local_host::save_private_json(&file, &saved)?;
                Ok((lease, saved))
            }).await??;
            let listener = tokio::net::UnixListener::bind(&lease.renderer.socket)?;
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&lease.renderer.socket, std::fs::Permissions::from_mode(0o600))?; }
            Ok::<_, anyhow::Error>(Prepared { lease, listener, saved })
        }).await;
        match prepared {
            Ok(Ok(prepared)) => { let _ = cx.update(|window, cx| {
                match NativeBrowser::new(prepared, window, cx) {
                    Ok(browser) => cx.set_global(NativeGlobal(Some(browser))),
                    Err(error) => tracing::error!(target:"agent", operation="browser_start", %error),
                }
            }); }
            _ => tracing::error!(target:"agent", operation="browser_start", "Cannot initialize the native browser owner"),
        }
    }).detach();
    window.on_window_should_close(cx, |_, cx| {
        shutdown(cx);
        true
    });
}

pub fn shutdown(cx: &mut App) {
    let owner = if cx.has_global::<NativeGlobal>() {
        cx.global_mut::<NativeGlobal>().0.take()
    } else {
        None
    };
    if let Some(owner) = owner {
        owner.update(cx, |owner, _| owner.shutdown());
    }
}

pub fn renderer_id(cx: &App) -> String {
    cx.try_global::<NativeGlobal>()
        .and_then(|g| g.0.as_ref())
        .map(|b| b.read(cx).lease.renderer.id.clone())
        .unwrap_or_default()
}

pub fn hide(scope: &str, cx: &mut App) {
    let owner = cx.try_global::<NativeGlobal>().and_then(|g| g.0.clone());
    if let Some(owner) = owner {
        owner.update(cx, |owner, _| {
            if owner.visible.as_ref().is_some_and(|v| v.scope == scope) {
                owner.visible = None;
                owner.visibility();
            }
        });
    }
}

pub fn has_surface(scope: &str, renderer: &str, tab: &str, cx: &App) -> bool {
    let Some(owner) = cx.try_global::<NativeGlobal>().and_then(|g| g.0.as_ref()) else {
        return false;
    };
    let owner = owner.read(cx);
    owner.lease.renderer.id == renderer
        && owner.pages.get(scope).is_some_and(|p| p.active == tab)
        && owner
            .tabs
            .iter()
            .any(|t| t.saved.scope == scope && t.saved.id == tab)
}

pub fn has_focus(scope: &str, tab: &str, cx: &App) -> bool {
    cx.try_global::<NativeGlobal>()
        .and_then(|global| global.0.as_ref())
        .and_then(|owner| {
            owner
                .read(cx)
                .tabs
                .iter()
                .find(|view| view.saved.scope == scope && view.saved.id == tab)
        })
        .is_some_and(|tab| focus::has_focus(&tab.view.webview()))
}

/// A click delivered to GPUI must also return AppKit's keyboard destination.
/// Native WebKit clicks bypass this handler and retain the page's own focus.
pub fn focus_parent(cx: &App) {
    if let Some(owner) = cx
        .try_global::<NativeGlobal>()
        .and_then(|global| global.0.as_ref())
    {
        for tab in &owner.read(cx).tabs {
            focus::release(&tab.view.webview());
        }
    }
}

/// Layout, visibility and canonical CSS viewport are updated by the same owner.
/// A newly selected tab stays hidden until the PC has laid out its actual panel.
pub fn layout(scope: &str, tab: &str, bounds: Bounds<Pixels>, cx: &mut App) {
    let owner = cx.try_global::<NativeGlobal>().and_then(|g| g.0.clone());
    if let Some(owner) = owner {
        owner.update(cx, |owner, _| {
            owner.visible = Some(Visible {
                scope: scope.into(),
                id: tab.into(),
                bounds,
            });
            owner.visibility();
        });
    }
}

impl NativeBrowser {
    fn shutdown(&mut self) {
        self.refresh();
        self.stop.cancel();
        self.jobs.clear();
        self.effects.borrow_mut().clear();
        self.tabs.clear();
        self.images.clear();
        self.save.close();
    }
    fn new(prepared: Prepared, window: &mut Window, cx: &mut App) -> Result<Entity<Self>, String> {
        let Prepared {
            lease,
            listener,
            mut saved,
        } = prepared;
        saved.validate()?;
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
        let runtime = cx.global::<crate::Runtime>().clone();
        let directory = lease.state_directory().to_owned();
        runtime.closing.spawn_on(async move {
            while let Ok(mut saved) = saves.recv().await {
                while let Ok(newer) = saves.try_recv() { saved = newer; }
                let file = directory.join("native-browser.json");
                let result = tokio::task::spawn_blocking(move || agent_local_host::save_private_json(&file, &saved)).await;
                if !matches!(result, Ok(Ok(()))) { tracing::error!(target:"agent", operation="browser_save", "Cannot save native browser tabs"); }
            }
        }, &runtime.handle);
        let stop = tokio_util::sync::CancellationToken::new();
        let server_stop = stop.clone();
        runtime.closing.spawn_on(
            async move {
                serve(listener, calls, server_stop).await;
            },
            &runtime.handle,
        );
        let Saved {
            profile: profile_id,
            pages,
            tabs: restore,
        } = saved;
        let owner = cx.new(|_| Self {
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
        });
        owner.update(cx, |owner, _| -> Result<(), String> {
            for saved in restore {
                browser_url(&saved.url)?;
                let (raw, delegate, input) = build(
                    window,
                    &saved.scope,
                    &saved.id,
                    owner.profile.clone(),
                    owner.effects.clone(),
                    owner.changed.clone(),
                )?;
                raw.load_url(&saved.url).map_err(|e| e.to_string())?;
                owner.attach(saved, raw, delegate, input);
            }
            owner.visibility();
            owner.lease.publish().map_err(|e| e.to_string())
        })?;
        {
            let receiver = requests;
            let weak = owner.downgrade();
            window
                .spawn(cx, async move |cx| {
                    while let Ok(call) = receiver.recv().await {
                        if weak
                            .update_in(cx, |owner, window, cx| owner.execute(call, window, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
        }
        let weak = owner.downgrade();
        window
            .spawn(cx, async move |cx| {
                while let Ok(event) = events.recv().await {
                    if weak
                        .update_in(cx, |owner, _, _| {
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
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
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
            let visible = self.visible.as_ref().filter(|v| {
                v.scope == tab.saved.scope && v.id == tab.saved.id && page.active == tab.saved.id
            });
            if let Some(visible) = visible {
                let bounds = visible.bounds;
                let scale = (f64::from(bounds.size.width / px(1.)) / f64::from(page.width))
                    .min(f64::from(bounds.size.height / px(1.)) / f64::from(page.height));
                if scale <= 0. {
                    focus::hide(&view.webview());
                    continue;
                }
                let width = f64::from(page.width) * scale;
                let height = f64::from(page.height) * scale;
                let _ = view.zoom(scale);
                let _ = view.set_bounds(wry::Rect {
                    position: wry::dpi::LogicalPosition::new(
                        f64::from(bounds.left() / px(1.))
                            + (f64::from(bounds.size.width / px(1.)) - width) / 2.,
                        f64::from(bounds.top() / px(1.))
                            + (f64::from(bounds.size.height / px(1.)) - height) / 2.,
                    )
                    .into(),
                    size: wry::dpi::LogicalSize::new(width, height).into(),
                });
                let _ = view.set_visible(true);
            } else {
                focus::hide(&view.webview());
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
    fn open(&mut self, scope: &str, url: &str, window: &mut Window) -> Result<(), String> {
        if self.tabs.iter().filter(|t| t.saved.scope == scope).count() >= 24 {
            return Err("タブは会話ごとに24個までです。".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (raw, delegate, input) = build(
            window,
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
    fn execute(&mut self, call: Call, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh();
        let request = call.request;
        let outcome = self.begin(&request, window);
        let ticket = match outcome {
            Ok(ticket) => ticket,
            Err(error) => {
                let _ = call.reply.try_send(Err(error));
                return;
            }
        };
        self.jobs.retain(|(_, job)| !job.is_ready());
        let job_tab = ticket.as_ref().map(|t| t.id.clone());
        let owner = cx.entity().downgrade();
        let task = cx.spawn_in(window, async move |_, cx| {
            // A page can return a Promise that never settles. Bound its Rust
            // waiter without stopping the page or replaying the operation.
            let deadline=cx.background_executor().timer(Duration::from_secs(30));
            let result=match futures_util::future::select(Box::pin(run(&owner,&request,ticket,cx)),Box::pin(deadline)).await {
                futures_util::future::Either::Left((result,_))=>result,
                futures_util::future::Either::Right(_)=>Err("ブラウザの応答待ちを終了しました。操作が完了している可能性があるため、画面を確認してください。".into()),
            };
            let _ = call.reply.try_send(result);
        });
        self.jobs.push((job_tab, task));
    }
    fn begin(
        &mut self,
        request: &NativeRequest,
        window: &mut Window,
    ) -> Result<Option<Ticket>, String> {
        request.validate()?;
        if self.jobs.iter().filter(|(_, j)| !j.is_ready()).count() >= 32 {
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
                BrowserAction::OpenTab { url } => self.open(scope, url, window)?,
                BrowserAction::Navigate { url } if page.active.is_empty() => {
                    self.open(scope, url, window)?
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
impl Drop for NativeBrowser {
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
    owner: &WeakEntity<NativeBrowser>,
    request: &NativeRequest,
    ticket: Option<Ticket>,
    cx: &mut AsyncWindowContext,
) -> NativeReply {
    if let NativeAction::ImportCookies(cookies) = &request.action {
        let store = owner
            .update_in(cx, |owner, _, _| unsafe {
                owner.profile.websiteDataStore().httpCookieStore()
            })
            .map_err(|_| "ブラウザが閉じています。")?;
        cookies::install(&store, cookies.clone()).await?;
    }
    let Some(ticket) = ticket else {
        if matches!(request.action, NativeAction::FillPassword { .. }) {
            return Err("先にログイン先のURLを開いてください。".into());
        }
        return owner
            .update_in(cx, |o, _, _| o.frame(&request.scope))
            .map_err(|_| "ブラウザが閉じています。".into());
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
                owner
                    .update_in(cx, |o, _, _| o.check(&ticket))
                    .map_err(|_| "ブラウザが閉じています。")??;
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
                platform::pointer(&ticket.view, &ticket.input, &events).await?;
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
                    owner
                        .update_in(cx, |o, _, _| o.check(&ticket))
                        .map_err(|_| "ブラウザが閉じています。")??;
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
                        owner
                            .update_in(cx, |o, _, _| o.check(&ticket))
                            .map_err(|_| "ブラウザが閉じています。")??;
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
    ) {
        read("true", true).await?;
    }
    let mut frame = capture_frame(
        owner,
        &request.scope,
        request.capture,
        &request.renderer_id,
        &request.image_id,
        cx,
    )
    .await?;
    frame.value = value;
    Ok(frame)
}

async fn capture_frame(
    owner: &WeakEntity<NativeBrowser>,
    scope: &str,
    capture: Capture,
    renderer: &str,
    image_id: &str,
    cx: &mut AsyncWindowContext,
) -> NativeReply {
    // Only observations are retried when a navigation overlaps a capture. The
    // operation above has already been dispatched once and is never repeated.
    for _ in 0..3 {
        let (ticket, mut frame) = owner
            .update_in(cx, |owner, _, _| {
                owner.refresh();
                (owner.ticket(scope), owner.frame(scope))
            })
            .map_err(|_| "ブラウザが閉じています。")?;
        let Some(ticket) = ticket else {
            return Ok(frame);
        };
        let result = async {
            if matches!(capture, Capture::Snapshot) && frame.dialog.is_none() {
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
            if frame.dialog.is_none()
                && (matches!(capture, Capture::Snapshot)
                    || (matches!(capture, Capture::Viewer) && renderer != frame.renderer_id))
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
        }
        .await;
        let valid = owner
            .update_in(cx, |owner, _, _| {
                owner.refresh();
                owner.check(&ticket)?;
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
            })
            .map_err(|_| "ブラウザが閉じています。")?;
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
    owner
        .update_in(cx, |owner, _, _| {
            owner.refresh();
            owner.frame(scope)
        })
        .map_err(|_| "ブラウザが閉じています。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn restored_tabs_have_unique_ids_and_belong_to_their_saved_conversation() {
        let mut saved = Saved::default();
        assert!(saved.validate().is_ok());
        saved.pages.insert(
            "a".into(),
            Page {
                active: "tab-a".into(),
                ..Default::default()
            },
        );
        saved.tabs.push(SavedTab {
            scope: "a".into(),
            id: "tab-a".into(),
            url: "https://example.test/".into(),
            opener: None,
        });
        assert!(saved.validate().is_ok());
        let mut invalid = saved.clone();
        invalid.profile = uuid::Uuid::nil().to_string();
        assert!(invalid.validate().is_err());
        let mut invalid = saved.clone();
        invalid.tabs[0].scope = "b".into();
        assert!(invalid.validate().is_err());
        let mut invalid = saved.clone();
        invalid.tabs.push(invalid.tabs[0].clone());
        assert!(invalid.validate().is_err());
        let mut invalid = saved.clone();
        invalid.pages.get_mut("a").unwrap().active = "tab-b".into();
        assert!(invalid.validate().is_err());
        let mut invalid = saved.clone();
        invalid.pages.get_mut("a").unwrap().width = 0;
        assert!(invalid.validate().is_err());
        saved.tabs[0].url = "file:///private/data".into();
        assert!(saved.validate().is_err());
    }
}
