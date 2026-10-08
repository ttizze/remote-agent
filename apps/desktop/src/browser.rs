use agent_core::{connection::Store, state::Intent};
use agent_protocol::browser::{browser_url, BrowserAction, BrowserFrame, BrowserRequest};
use agent_protocol::preview::{
    DiscoveredLocalServer, PreviewAppearance, PreviewViewportSetting, PreviewZoom,
};
#[cfg(target_os = "macos")]
use gpui_kit::component::{
    Disableable,
    menu::{DropdownMenu, PopupMenuItem},
};
use gpui_kit::{
    component::{
        IconName, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use gpui_wry::WebView;
use std::sync::Arc;

enum Event {
    Location(String),
    Navigate(String),
}

#[cfg(target_os = "macos")]
pub(crate) struct ChromeProfileSource {
    pub(crate) root: Option<std::path::PathBuf>,
    pub(crate) password: fn() -> Result<Vec<u8>, String>,
}

#[cfg(target_os = "macos")]
impl Default for ChromeProfileSource {
    fn default() -> Self {
        Self {
            root: directories::BaseDirs::new().map(|dirs| {
                dirs.home_dir()
                    .join("Library/Application Support/Google/Chrome")
            }),
            password: || {
                security_framework::passwords::get_generic_password("Chrome Safe Storage", "Chrome")
                    .map_err(|_| {
                        "Can't access Chrome's Keychain item. Allow access and try again.".into()
                    })
            },
        }
    }
}

pub(crate) struct Browser {
    webview: Entity<WebView>,
    address: Entity<InputState>,
    error: String,
    #[cfg(target_os = "macos")]
    importing: bool,
    #[cfg(target_os = "macos")]
    import_notice: String,
    #[cfg(target_os = "macos")]
    chrome: ChromeProfileSource,
    #[cfg(target_os = "macos")]
    visible: bool,
    #[cfg(target_os = "macos")]
    profile_menu_open: bool,
    _subscription: Subscription,
}

impl Browser {
    pub(crate) fn new(
        builder: wry::WebViewBuilder<'_>,
        #[cfg(target_os = "macos")] chrome: ChromeProfileSource,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, String> {
        let (tx, rx) = async_channel::unbounded();
        let navigation = tx.clone();
        let raw = builder
            .with_url("about:blank")
            .with_background_color((24, 24, 24, 255))
            .with_focused(false)
            .with_accept_first_mouse(true)
            // A browsing page never receives the terminal's privileged IPC bridge.
            .with_navigation_handler(|url| browser_url(&url).is_ok())
            .with_new_window_req_handler(move |url, _| {
                let _ = navigation.try_send(Event::Navigate(url));
                wry::NewWindowResponse::Deny
            })
            .with_on_page_load_handler(move |_, url| {
                let _ = tx.try_send(Event::Location(url));
            })
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        let webview = cx.new(|cx| WebView::new(raw, window, cx));
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter URL"));
        Ok(cx.new(|cx: &mut Context<Self>| {
            let subscription = cx.subscribe_in(&address, window, |s, _, event, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    s.navigate(cx);
                }
            });
            cx.spawn_in(window, async move |view, cx| {
                while let Ok(event) = rx.recv().await {
                    if view
                        .update_in(cx, |s, window, cx| {
                            match event {
                                Event::Location(url) => s.address.update(cx, |input, cx| {
                                    input.set_value(
                                        if url == "about:blank" {
                                            String::new()
                                        } else {
                                            url
                                        },
                                        window,
                                        cx,
                                    )
                                }),
                                Event::Navigate(url) => {
                                    s.address
                                        .update(cx, |input, cx| input.set_value(url, window, cx));
                                    s.navigate(cx);
                                }
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Self {
                webview,
                address,
                error: String::new(),
                #[cfg(target_os = "macos")]
                importing: false,
                #[cfg(target_os = "macos")]
                import_notice: String::new(),
                #[cfg(target_os = "macos")]
                chrome,
                #[cfg(target_os = "macos")]
                visible: true,
                #[cfg(target_os = "macos")]
                profile_menu_open: false,
                _subscription: subscription,
            }
        }))
    }
    /// The tab title: the page's host, else "Browser".
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn title(&self, cx: &App) -> String {
        url::Url::parse(&self.address.read(cx).value())
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "Browser".into())
    }
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut App) {
        #[cfg(target_os = "macos")]
        {
            self.visible = visible;
        }
        #[cfg(target_os = "macos")]
        let visible = visible && !self.profile_menu_open;
        self.webview.update(cx, |view, _| {
            if view.visible() != visible {
                if visible {
                    view.show();
                } else {
                    view.hide();
                }
            }
        });
    }
    fn navigate(&mut self, cx: &mut Context<Self>) {
        match browser_url(self.address.read(cx).value().as_ref()) {
            Ok(url) => {
                self.error.clear();
                if let Err(e) = self.webview.read(cx).raw().load_url(&url) {
                    self.error = e.to_string();
                }
            }
            Err(e) => self.error = e,
        }
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn import_chrome(
        &mut self,
        profile: std::path::PathBuf,
        password: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        use futures_util::{FutureExt, future::Either};
        use wry::WebViewExtMacOS;
        if self.importing {
            return;
        }
        self.importing = true;
        self.error.clear();
        self.import_notice = "Reading Chrome cookies… Allow Keychain access if asked.".into();
        let read = cx.background_executor().spawn(async move {
            read_chrome_cookies(&profile, chrono::Utc::now().timestamp(), password)
        });
        cx.spawn(async move |owner, cx| {
            let result = async {
                let (cookies, skipped) = read.await?;
                let count = cookies.len();
                if count == 0 {
                    return Ok(format!("No cookies to import ({skipped} expired or unsupported skipped)."));
                }
                let store = owner.update(cx, |s, cx| unsafe {
                    s.webview.read(cx).raw().webview().configuration().websiteDataStore().httpCookieStore()
                }).map_err(|_| "The browser was closed.".to_string())?;
                let timer = cx.background_executor().timer(std::time::Duration::from_secs(30));
                match futures_util::future::select(install_chrome_cookies(&store, cookies).boxed_local(), timer.boxed_local()).await {
                    Either::Left((result, _)) => result?,
                    Either::Right(_) => return Err("Saving cookies timed out. Some may have been saved. Try again.".into()),
                }
                owner.update(cx, |s, cx| s.webview.read(cx).raw().reload())
                    .map_err(|_| "The browser was closed.".to_string())?
                    .map_err(|_| "Cookies were saved, but the page didn't reload. Reload it.".to_string())?;
                Ok(format!("Imported {count} cookies ({skipped} expired or unsupported skipped). Check the site to confirm you're signed in."))
            }.await;
            let _ = owner.update(cx, |s, cx| {
                s.importing = false;
                match result {
                    Ok(notice) => s.import_notice = notice,
                    Err(error) => { s.import_notice.clear(); s.error = error; }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn chrome_import_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let visibility = owner.clone();
        let root = self.chrome.root.clone();
        let password = self.chrome.password;
        Button::new("import-chrome-cookies")
            .label(if self.importing {
                "Importing…"
            } else {
                "Import from Chrome"
            })
            .accessibility_label("Import cookies from Chrome")
            .small()
            .ghost()
            .disabled(self.importing)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                let profiles = root
                    .as_deref()
                    .ok_or_else(|| "Can't find the home folder.".to_string())
                    .and_then(chrome_profiles);
                match profiles {
                    Ok(profiles) => {
                        for (name, path) in profiles {
                            let owner = owner.clone();
                            menu = menu.item(PopupMenuItem::new(name).on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |s, cx| {
                                    s.import_chrome(path.clone(), password, cx)
                                });
                            }));
                        }
                    }
                    Err(error) => {
                        let owner = owner.clone();
                        menu = menu.item(PopupMenuItem::new(error.clone()).on_click(
                            move |_, _, cx| {
                                let _ = owner.update(cx, |s, cx| {
                                    s.error = error.clone();
                                    cx.notify();
                                });
                            },
                        ));
                    }
                }
                menu
            })
            .on_open_change(move |open, _, cx| {
                let _ = visibility.update(cx, |s, cx| {
                    // Native WebViews sit above GPUI popovers; retain the panel's requested visibility.
                    s.profile_menu_open = *open;
                    s.set_visible(s.visible, cx);
                    cx.notify();
                });
            })
    }
}

enum HostBrowserRequest {
    Open,
    Action(BrowserAction),
    Intent(Intent),
}

/// Renders frames from the Host-owned Preview browser. The image and every
/// input action share the Host tab identity, so a panel switch never creates a
/// second local page behind the user's visible Preview.
pub(crate) struct HostBrowser {
    store: Arc<Store>,
    thread_id: String,
    frame: Option<BrowserFrame>,
    image: Option<Arc<Image>>,
    local_servers: Vec<DiscoveredLocalServer>,
    recent_urls: Vec<String>,
    address: Entity<InputState>,
    focus: FocusHandle,
    frame_bounds: Bounds<Pixels>,
    error: String,
    _subscription: Subscription,
}

impl HostBrowser {
    pub(crate) fn new(
        store: Arc<Store>,
        thread_id: String,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Enter preview URL"));
        cx.new(|cx: &mut Context<Self>| {
            let subscription = cx.subscribe_in(&address, window, |view, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    view.navigate(window, cx);
                }
            });
            let mut view = Self {
                store,
                thread_id,
                frame: None,
                image: None,
                local_servers: Vec::new(),
                recent_urls: Vec::new(),
                address,
                focus: cx.focus_handle(),
                frame_bounds: Bounds::default(),
                error: String::new(),
                _subscription: subscription,
            };
            view.request(HostBrowserRequest::Open, window, cx);
            view
        })
    }

    pub(crate) fn title(&self, _: &App) -> String {
        self.frame
            .as_ref()
            .and_then(|frame| frame.tabs.iter().find(|tab| tab.id == frame.tab_id))
            .and_then(|tab| url::Url::parse(&tab.url).ok())
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "Preview".into())
    }

    pub(crate) fn set_visible(&mut self, _: bool, _: &mut Context<Self>) {}

    pub(crate) fn close(&self) {
        let store = self.store.clone();
        let tab_id = self.frame.as_ref().map(|frame| frame.tab_id.clone());
        tokio::spawn(async move {
            if let Some(tab_id) = tab_id {
                let _ = store
                    .dispatch(Intent::PreviewClose {
                        tab_id: Some(tab_id),
                    })
                    .await;
            }
        });
    }

    fn navigate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.address.read(cx).value();
        let url = match agent_protocol::preview::normalize_preview_url(&value) {
            Ok(url) => url,
            Err(error) => {
                self.error = error;
                cx.notify();
                return;
            }
        };
        self.request(
            HostBrowserRequest::Action(BrowserAction::Navigate { url }),
            window,
            cx,
        );
    }

    fn request(
        &mut self,
        request: HostBrowserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store.clone();
        let thread_id = self.thread_id.clone();
        let frame = self.frame.clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = host_browser_request(store, thread_id, frame, request).await;
            let _ = view.update_in(cx, |view, window, cx| {
                match result {
                    Ok(frame) => view.apply_frame(frame, window, cx),
                    Err(error) => {
                        view.error = error;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn apply_frame(&mut self, frame: BrowserFrame, window: &mut Window, cx: &mut Context<Self>) {
        self.error.clear();
        let preview = self.store.snapshot().preview.clone();
        self.local_servers = preview.local_servers;
        self.recent_urls = preview.recent_urls;
        if let Some(tab) = frame.tabs.iter().find(|tab| tab.id == frame.tab_id) {
            let value = if tab.url == "about:blank" {
                String::new()
            } else {
                tab.url.clone()
            };
            self.address
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if !frame.image.is_empty() {
            self.image = Some(Arc::new(Image::from_bytes(
                ImageFormat::Jpeg,
                frame.image.clone(),
            )));
        }
        self.frame = Some(frame);
        cx.notify();
    }

    fn frame_point(&self, point: Point<Pixels>) -> (f64, f64) {
        let Some(frame) = self.frame.as_ref() else {
            return (0.0, 0.0);
        };
        let width = self.frame_bounds.size.width.as_f32().max(1.0);
        let height = self.frame_bounds.size.height.as_f32().max(1.0);
        let scale = (width / frame.width as f32).min(height / frame.height as f32);
        let rendered_width = frame.width as f32 * scale;
        let rendered_height = frame.height as f32 * scale;
        let offset_x = (width - rendered_width) / 2.0;
        let offset_y = (height - rendered_height) / 2.0;
        let x = (point.x.as_f32() - self.frame_bounds.left().as_f32() - offset_x)
            .clamp(0.0, rendered_width);
        let y = (point.y.as_f32() - self.frame_bounds.top().as_f32() - offset_y)
            .clamp(0.0, rendered_height);
        (
            f64::from(x) / f64::from(scale),
            f64::from(y) / f64::from(scale),
        )
    }
}

fn next_viewport(current: PreviewViewportSetting) -> PreviewViewportSetting {
    use agent_protocol::preview::PREVIEW_VIEWPORT_PRESETS;
    let make = |preset: agent_protocol::preview::PreviewViewportPresetDefinition| {
        PreviewViewportSetting::Preset {
            preset: preset.id,
            width: preset.width,
            height: preset.height,
        }
    };
    let Some(index) = PREVIEW_VIEWPORT_PRESETS.iter().position(|preset| {
        matches!(
            current,
            PreviewViewportSetting::Preset { preset: id, .. } if id == preset.id
        )
    }) else {
        return PREVIEW_VIEWPORT_PRESETS
            .first()
            .copied()
            .map_or(PreviewViewportSetting::Fill, make);
    };
    PREVIEW_VIEWPORT_PRESETS
        .get(index + 1)
        .copied()
        .map_or(PreviewViewportSetting::Fill, make)
}

fn next_appearance(current: PreviewAppearance) -> PreviewAppearance {
    match current {
        PreviewAppearance::System => PreviewAppearance::Light,
        PreviewAppearance::Light => PreviewAppearance::Dark,
        PreviewAppearance::Dark => PreviewAppearance::System,
    }
}

async fn host_browser_request(
    store: Arc<Store>,
    thread_id: String,
    frame: Option<BrowserFrame>,
    request: HostBrowserRequest,
) -> Result<BrowserFrame, String> {
    let sync_navigation = matches!(
        &request,
        HostBrowserRequest::Action(
            BrowserAction::Back | BrowserAction::Forward | BrowserAction::Reload
        )
    );
    let tab_id = frame.as_ref().map(|frame| frame.tab_id.clone()).unwrap_or_default();
    let image_id = frame
        .as_ref()
        .map(|frame| frame.image_id.clone())
        .unwrap_or_default();
    let action = match request {
        HostBrowserRequest::Open => {
            dispatch_preview(
                &store,
                Intent::PreviewOpen {
                    url: None,
                    viewport: PreviewViewportSetting::Fill,
                    appearance: PreviewAppearance::System,
                    zoom: PreviewZoom::X100,
                },
            )
            .await?;
            dispatch_preview(
                &store,
                Intent::PreviewList {
                    configured_urls: configured_preview_urls(&store),
                },
            )
            .await?;
            BrowserAction::Read
        }
        HostBrowserRequest::Action(BrowserAction::Navigate { url }) => {
            dispatch_preview(
                &store,
                Intent::PreviewNavigate {
                    tab_id: tab_id.clone(),
                    url,
                },
            )
            .await?;
            BrowserAction::Read
        }
        HostBrowserRequest::Intent(intent) => {
            dispatch_preview(&store, intent).await?;
            BrowserAction::Read
        }
        HostBrowserRequest::Action(action) => action,
    };
    let frame = store
        .browser(BrowserRequest {
            thread_id: agent_domain::ThreadId::new(thread_id)
                .map_err(|_| "thread is invalid".to_owned())?,
            tab_id,
            image_id,
            action,
        })
        .await
        .map_err(|error| error.to_string())?;
    if sync_navigation {
        let _ = dispatch_preview(
            &store,
            Intent::PreviewList {
                configured_urls: configured_preview_urls(&store),
            },
        )
        .await;
    }
    Ok(frame)
}

fn configured_preview_urls(store: &Store) -> Vec<String> {
    store
        .snapshot()
        .shell_projects()
        .iter()
        .flat_map(|project| &project.scripts)
        .filter_map(|script| script.preview_url.clone())
        .collect()
}

async fn dispatch_preview(store: &Store, intent: Intent) -> Result<(), String> {
    store
        .dispatch(intent)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    Ok(())
}

impl Render for HostBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = self.image.clone();
        let frame = self.frame.as_ref();
        let frame_owner = cx.entity().downgrade();
        let empty = frame
            .and_then(|frame| frame.tabs.iter().find(|tab| tab.id == frame.tab_id))
            .is_none_or(|tab| tab.url.is_empty() || tab.url == "about:blank");
        let local_servers = self.local_servers.clone();
        let recent_urls = self.recent_urls.clone();
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .gap_1()
                    .p_2()
                    .child(
                        Button::new("preview-back")
                            .icon(IconName::ArrowLeft)
                            .small()
                            .ghost()
                            .tooltip("Back")
                            .accessibility_label("Back")
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.request(
                                    HostBrowserRequest::Action(BrowserAction::Back),
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("preview-forward")
                            .icon(IconName::ArrowRight)
                            .small()
                            .ghost()
                            .tooltip("Forward")
                            .accessibility_label("Forward")
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.request(
                                    HostBrowserRequest::Action(BrowserAction::Forward),
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("preview-reload")
                            .icon(IconName::RotateCw)
                            .small()
                            .ghost()
                            .tooltip("Refresh")
                            .accessibility_label("Refresh")
                            .on_click(cx.listener(|s, _, window, cx| {
                                s.request(
                                    HostBrowserRequest::Action(BrowserAction::Reload),
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("preview-zoom-out")
                            .label("−")
                            .small()
                            .ghost()
                            .tooltip("Zoom out")
                            .accessibility_label("Zoom out")
                            .on_click(cx.listener(|s, _, window, cx| {
                                let Some(tab_id) = s.frame.as_ref().map(|frame| frame.tab_id.clone()) else { return };
                                let zoom = s
                                    .store
                                    .snapshot()
                                    .preview
                                    .session(&tab_id)
                                    .map_or(PreviewZoom::X100, |session| session.zoom)
                                    .stepped(-1);
                                s.request(
                                    HostBrowserRequest::Intent(Intent::PreviewSetZoom {
                                        tab_id,
                                        zoom,
                                    }),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("preview-zoom-in")
                            .label("+")
                            .small()
                            .ghost()
                            .tooltip("Zoom in")
                            .accessibility_label("Zoom in")
                            .on_click(cx.listener(|s, _, window, cx| {
                                let Some(tab_id) = s.frame.as_ref().map(|frame| frame.tab_id.clone()) else { return };
                                let zoom = s
                                    .store
                                    .snapshot()
                                    .preview
                                    .session(&tab_id)
                                    .map_or(PreviewZoom::X100, |session| session.zoom)
                                    .stepped(1);
                                s.request(
                                    HostBrowserRequest::Intent(Intent::PreviewSetZoom {
                                        tab_id,
                                        zoom,
                                    }),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("preview-viewport")
                            .label("Viewport")
                            .small()
                            .ghost()
                            .tooltip("Cycle viewport")
                            .accessibility_label("Cycle viewport")
                            .on_click(cx.listener(|s, _, window, cx| {
                                let Some(tab_id) = s.frame.as_ref().map(|frame| frame.tab_id.clone()) else { return };
                                let viewport = s
                                    .store
                                    .snapshot()
                                    .preview
                                    .session(&tab_id)
                                    .map_or(PreviewViewportSetting::Fill, |session| session.viewport);
                                s.request(
                                    HostBrowserRequest::Intent(Intent::PreviewResize {
                                        tab_id,
                                        viewport: next_viewport(viewport),
                                    }),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("preview-appearance")
                            .label("Theme")
                            .small()
                            .ghost()
                            .tooltip("Cycle appearance")
                            .accessibility_label("Cycle appearance")
                            .on_click(cx.listener(|s, _, window, cx| {
                                let Some(tab_id) = s.frame.as_ref().map(|frame| frame.tab_id.clone()) else { return };
                                let appearance = s
                                    .store
                                    .snapshot()
                                    .preview
                                    .session(&tab_id)
                                    .map_or(PreviewAppearance::System, |session| session.appearance);
                                s.request(
                                    HostBrowserRequest::Intent(Intent::PreviewSetAppearance {
                                        tab_id,
                                        appearance: next_appearance(appearance),
                                    }),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(Input::new(&self.address).small().aria_label("Preview URL"))
                    .child(
                        Button::new("preview-go")
                            .icon(IconName::ArrowRight)
                            .small()
                            .ghost()
                            .tooltip("Open")
                            .accessibility_label("Open")
                            .on_click(cx.listener(|s, _, window, cx| s.navigate(window, cx))),
                    ),
            )
            .when(!self.error.is_empty(), |body| {
                body.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_sm()
                        .text_color(rgb(0xff8e86))
                        .child(self.error.clone()),
                )
            })
            .child(
                div()
                    .id("preview-frame")
                    .on_prepaint(move |bounds, _, cx| {
                        let _ = frame_owner.update(cx, |view, _| view.frame_bounds = bounds);
                    })
                    .track_focus(&self.focus)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(rgb(0x181818))
                    .when(empty, |body| {
                        body.child(
                            v_flex()
                                .gap_1()
                                .p_4()
                                .max_w(px(420.))
                                .child(div().text_sm().child("Open a local preview"))
                                .children(local_servers.iter().map(|server| {
                                    let url = server.url.clone();
                                    h_flex()
                                        .id(SharedString::from(format!("preview-local-{}", server.port)))
                                        .w_full()
                                        .gap_2()
                                        .p_2()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .hover(|row| row.bg(rgb(0x2a2a2a)))
                                        .child(server.url.clone())
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            s.request(
                                                HostBrowserRequest::Action(BrowserAction::Navigate {
                                                    url: url.clone(),
                                                }),
                                                window,
                                                cx,
                                            )
                                        }))
                                }))
                                .when(!recent_urls.is_empty(), |body| {
                                    body.child(div().pt_2().text_xs().child("Recent"))
                                        .children(recent_urls.iter().map(|url| {
                                            let url = url.clone();
                                            div()
                                                .id(SharedString::from(format!("preview-recent-{url}")))
                                                .w_full()
                                                .p_1()
                                                .cursor_pointer()
                                                .text_xs()
                                                .child(url.clone())
                                                .on_click(cx.listener(move |s, _, window, cx| {
                                                    s.request(
                                                        HostBrowserRequest::Action(
                                                            BrowserAction::Navigate { url: url.clone() },
                                                        ),
                                                        window,
                                                        cx,
                                                    )
                                                }))
                                        }))
                                }),
                        )
                    })
                    .when_some(image, |body, image| {
                        body.child(img(image).size_full().object_fit(ObjectFit::Contain))
                    })
                    .when(frame.is_some(), |body| {
                        body.on_key_down(cx.listener(|s, event: &KeyDownEvent, window, cx| {
                            let key = event.keystroke.key.as_str();
                            let modifiers = event.keystroke.modifiers;
                            let action = if (modifiers.platform || modifiers.control) && key == "a" {
                                Some(BrowserAction::Key {
                                    key: agent_protocol::browser::BrowserKey::SelectAll,
                                })
                            } else if (modifiers.platform || modifiers.control) && key == "v" {
                                cx.read_from_clipboard().and_then(|text| {
                                    text.text().map(|text| BrowserAction::Type { text })
                                })
                            } else {
                                match key {
                                    "enter" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::Enter,
                                    }),
                                    "tab" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::Tab,
                                    }),
                                    "backspace" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::Backspace,
                                    }),
                                    "escape" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::Escape,
                                    }),
                                    "up" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::ArrowUp,
                                    }),
                                    "down" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::ArrowDown,
                                    }),
                                    "left" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::ArrowLeft,
                                    }),
                                    "right" => Some(BrowserAction::Key {
                                        key: agent_protocol::browser::BrowserKey::ArrowRight,
                                    }),
                                    value if !modifiers.control
                                        && !modifiers.alt
                                        && !modifiers.platform
                                        && value.chars().count() == 1 => {
                                        Some(BrowserAction::Type {
                                            text: value.to_owned(),
                                        })
                                    }
                                    _ => None,
                                }
                            };
                            if let Some(action) = action {
                                cx.stop_propagation();
                                s.request(HostBrowserRequest::Action(action), window, cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|s, event: &MouseDownEvent, window, cx| {
                                s.focus.focus(window, cx);
                                let (x, y) = s.frame_point(event.position);
                                s.request(
                                    HostBrowserRequest::Action(BrowserAction::Click {
                                        x,
                                        y,
                                    }),
                                    window,
                                    cx,
                                )
                            })
                        )
                        .on_scroll_wheel(cx.listener(|s, event: &ScrollWheelEvent, window, cx| {
                            let delta = event.delta.pixel_delta(px(1.));
                            let (x, y) = s.frame_point(event.position);
                            s.request(
                                HostBrowserRequest::Action(BrowserAction::Scroll {
                                    x,
                                    y,
                                    delta_x: delta.x.as_f32() as f64,
                                    delta_y: delta.y.as_f32() as f64,
                                }),
                                window,
                                cx,
                            )
                        }))
                    }),
            )
    }
}

impl Render for Browser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .gap_1()
                    .p_2()
                    .child(
                        Button::new("browser-back")
                            .icon(IconName::ArrowLeft)
                            .small()
                            .ghost()
                            .tooltip("Back")
                            .accessibility_label("Back")
                            .on_click(cx.listener(|s, _, _, cx| {
                                let _ = s.webview.read(cx).raw().evaluate_script("history.back()");
                            })),
                    )
                    .child(
                        Button::new("browser-forward")
                            .icon(IconName::ArrowRight)
                            .small()
                            .ghost()
                            .tooltip("Forward")
                            .accessibility_label("Forward")
                            .on_click(cx.listener(|s, _, _, cx| {
                                let _ = s
                                    .webview
                                    .read(cx)
                                    .raw()
                                    .evaluate_script("history.forward()");
                            })),
                    )
                    .child(
                        Button::new("browser-reload")
                            .icon(IconName::RotateCw)
                            .small()
                            .ghost()
                            .tooltip("Refresh")
                            .accessibility_label("Refresh")
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Err(e) = s.webview.read(cx).raw().reload() {
                                    s.error = e.to_string();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Input::new(&self.address)
                            .small()
                            .aria_label("Search or enter URL"),
                    )
                    .child(
                        Button::new("browser-go")
                            .icon(IconName::ArrowRight)
                            .small()
                            .ghost()
                            .tooltip("Go")
                            .accessibility_label("Go")
                            .on_click(cx.listener(|s, _, _, cx| s.navigate(cx))),
                    ),
            )
            .map(|body| {
                #[cfg(target_os = "macos")]
                let body = body
                    .child(h_flex().px_2().pb_2().child(self.chrome_import_button(cx)))
                    .when(!self.import_notice.is_empty(), |body| {
                        body.child(
                            div()
                                .px_3()
                                .pb_2()
                                .text_sm()
                                .child(self.import_notice.clone()),
                        )
                    });
                body
            })
            .when(!self.error.is_empty(), |body| {
                body.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_sm()
                        .text_color(rgb(0xff8e86))
                        .child(self.error.clone()),
                )
            })
            .child(div().flex_1().min_h_0().pl_1().child(self.webview.clone()))
    }
}

#[cfg(target_os = "macos")]
fn chrome_profiles(root: &std::path::Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let bytes = std::fs::read(root.join("Local State"))
        .map_err(|_| "No Chrome profile found. Open Chrome once, then try again.".to_string())?;
    let state: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "Can't read Chrome's profile list.".to_string())?;
    let profiles = state
        .pointer("/profile/info_cache")
        .and_then(|v| v.as_object())
        .ok_or("Chrome has no profile list.")?;
    let mut result = Vec::new();
    for (directory, info) in profiles {
        // Local State must not redirect the importer outside Chrome's profile root.
        let mut components = std::path::Path::new(directory).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            continue;
        }
        let profile = root.join(directory);
        if chrome_cookie_path(&profile).is_some() {
            let name = info
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(directory);
            result.push((format!("{name} ({directory})"), profile));
        }
    }
    if result.is_empty() {
        return Err("No Chrome profile has cookies.".into());
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
fn chrome_cookie_path(profile: &std::path::Path) -> Option<std::path::PathBuf> {
    ["Network/Cookies", "Cookies"]
        .into_iter()
        .map(|name| profile.join(name))
        .find(|path| path.is_file())
}

#[cfg(target_os = "macos")]
struct ChromeCookie {
    domain: String,
    name: String,
    value: zeroize::Zeroizing<String>,
    path: String,
    secure: bool,
    http_only: bool,
    same_site: i64,
    expires: Option<i64>,
}

#[cfg(target_os = "macos")]
fn read_chrome_cookies(
    profile: &std::path::Path,
    now: i64,
    password: impl FnOnce() -> Result<Vec<u8>, String>,
) -> Result<(Vec<ChromeCookie>, usize), String> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    use sha2::{Digest, Sha256};
    use zeroize::Zeroizing;
    let path = chrome_cookie_path(profile).ok_or("Chrome's cookie file is missing.")?;
    let mut db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| {
                "Can't open Chrome's cookies. Check the file's permissions.".to_string()
            })?;
    db.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| "Couldn't prepare to read cookies.")?;
    // One read transaction includes committed WAL rows without copying or modifying Chrome's DB.
    let transaction = db
        .transaction()
        .map_err(|_| "Can't read Chrome's cookies. Try again.")?;
    let schema: i64 = transaction
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            row.get::<_, String>(0)?
                .parse::<i64>()
                .map_err(|_| rusqlite::Error::InvalidQuery)
        })
        .map_err(|_| "Can't identify Chrome's cookie format.")?;
    if !matches!(schema, 23 | 24) {
        return Err("This version of Chrome's cookie format isn't supported yet.".into());
    }
    let mut statement = transaction.prepare("SELECT host_key, name, value, encrypted_value, path, is_secure, is_httponly, samesite, expires_utc, has_expires, top_frame_site_key FROM cookies")
        .map_err(|_| "Can't read Chrome's cookie format.")?;
    let rows = statement
        .query_map([], |row| {
            let expires: i64 = row.get(8)?;
            Ok((
                ChromeCookie {
                    domain: row.get(0)?,
                    name: row.get(1)?,
                    value: Zeroizing::new(row.get(2)?),
                    path: row.get(4)?,
                    secure: row.get(5)?,
                    http_only: row.get(6)?,
                    same_site: row.get(7)?,
                    expires: if row.get::<_, bool>(9)? {
                        Some(expires / 1_000_000 - 11_644_473_600)
                    } else {
                        None
                    },
                },
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(10)?,
            ))
        })
        .map_err(|_| "Can't read Chrome's cookies.")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Some of Chrome's cookies can't be read.")?;
    drop(statement);
    drop(transaction);
    drop(db);
    let mut password = Some(password);
    let mut key = None;
    let mut cookies = Vec::new();
    let mut skipped = 0;
    let mut identities = std::collections::HashSet::new();
    for (mut cookie, encrypted, partition) in rows {
        if !partition.is_empty() || cookie.expires.is_some_and(|expires| expires <= now) {
            skipped += 1;
            continue;
        }
        if !encrypted.is_empty() {
            if !cookie.value.is_empty() {
                return Err("Chrome's cookies contain conflicting values.".into());
            }
            let encrypted = encrypted
                .strip_prefix(b"v10")
                .ok_or("Unsupported cookie encryption. Nothing was imported.")?;
            if key.is_none() {
                let secret = Zeroizing::new(password.take().expect("key requested once")()?);
                let mut derived = Zeroizing::new([0u8; 16]);
                pbkdf2::pbkdf2_hmac::<sha1::Sha1>(&secret, b"saltysalt", 1003, derived.as_mut());
                key = Some(derived);
            }
            let key = key.as_ref().expect("derived key");
            let clear = Zeroizing::new(
                cbc::Decryptor::<aes::Aes128>::new((&**key).into(), (&[b' '; 16]).into())
                    .decrypt_padded_vec_mut::<Pkcs7>(encrypted)
                    .map_err(|_| "Can't decrypt Chrome's cookies. Nothing was imported.")?,
            );
            let value = if schema >= 24 {
                if clear.len() < 32 || clear[..32] != Sha256::digest(cookie.domain.as_bytes())[..] {
                    return Err(
                        "A Chrome cookie failed its domain check. Nothing was imported.".into(),
                    );
                }
                &clear[32..]
            } else {
                &clear[..]
            };
            cookie.value = Zeroizing::new(
                std::str::from_utf8(value)
                    .map_err(|_| "A Chrome cookie uses an unsupported encoding.")?
                    .to_owned(),
            );
        }
        if !matches!(cookie.same_site, -1..=2)
            || cookie.domain.is_empty()
            || !cookie.path.starts_with('/')
            || cookie
                .name
                .bytes()
                .any(|b| b <= 0x20 || b >= 0x7f || b"()<>@,;:\\\"/[]?={}".contains(&b))
            || cookie
                .value
                .bytes()
                .any(|b| b < 0x20 || b == 0x7f || b == b';')
            || url::Host::parse(cookie.domain.trim_start_matches('.')).is_err()
        {
            return Err(
                "A Chrome cookie has an unsupported attribute. Nothing was imported.".into(),
            );
        }
        if !identities.insert((
            cookie.domain.clone(),
            cookie.name.clone(),
            cookie.path.clone(),
        )) {
            return Err(
                "Some cookies are duplicates the browser can't tell apart. Nothing was imported."
                    .into(),
            );
        }
        cookies.push(cookie);
    }
    Ok((cookies, skipped))
}

#[cfg(target_os = "macos")]
fn native_chrome_cookie(
    cookie: &ChromeCookie,
) -> Result<objc2::rc::Retained<objc2_foundation::NSHTTPCookie>, String> {
    use objc2::runtime::AnyObject;
    use objc2_foundation::*;
    // Preserve Chrome's leading dot: cookie::Cookie::domain() strips it and changes host scope.
    unsafe {
        let properties: objc2::rc::Retained<
            NSMutableDictionary<NSHTTPCookiePropertyKey, AnyObject>,
        > = NSMutableDictionary::from_slices(
            &[
                NSHTTPCookieName,
                NSHTTPCookieValue,
                NSHTTPCookieDomain,
                NSHTTPCookiePath,
            ],
            &[
                &*NSString::from_str(&cookie.name),
                &*NSString::from_str(&cookie.value),
                &*NSString::from_str(&cookie.domain),
                &*NSString::from_str(&cookie.path),
            ],
        );
        if cookie.secure {
            properties.insert(NSHTTPCookieSecure, ns_string!("TRUE"));
        }
        if cookie.http_only {
            properties.insert(ns_string!("HttpOnly"), ns_string!("TRUE"));
        }
        if let Some(expires) = cookie.expires {
            properties.insert(
                NSHTTPCookieExpires,
                &*NSDate::dateWithTimeIntervalSince1970(expires as f64),
            );
        } else {
            properties.insert(NSHTTPCookieDiscard, ns_string!("TRUE"));
        }
        match cookie.same_site {
            0 => {
                properties.insert(NSHTTPCookieSameSitePolicy, ns_string!("none"));
            }
            // WebKit interprets nil as unrestricted; preserve Chrome's Lax-by-default policy.
            -1 | 1 => {
                properties.insert(NSHTTPCookieSameSitePolicy, NSHTTPCookieSameSiteLax);
            }
            2 => {
                properties.insert(NSHTTPCookieSameSitePolicy, NSHTTPCookieSameSiteStrict);
            }
            _ => {}
        }
        NSHTTPCookie::cookieWithProperties(&properties).ok_or_else(|| {
            "Some cookies can't be passed to the browser. Nothing was imported.".into()
        })
    }
}

#[cfg(target_os = "macos")]
async fn install_chrome_cookies(
    store: &objc2_web_kit::WKHTTPCookieStore,
    cookies: Vec<ChromeCookie>,
) -> Result<(), String> {
    // Validate the whole batch before updating any existing browser cookie.
    let native = cookies
        .into_iter()
        .map(|cookie| native_chrome_cookie(&cookie))
        .collect::<Result<Vec<_>, _>>()?;
    let (tx, rx) = async_channel::unbounded();
    for cookie in &native {
        let tx = tx.clone();
        unsafe {
            store.setCookie_completionHandler(
                cookie,
                Some(&block2::RcBlock::new(move || {
                    let _ = tx.try_send(());
                })),
            );
        }
        rx.recv()
            .await
            .map_err(|_| "Can't confirm the cookies were saved.")?;
    }
    let (tx, rx) = async_channel::bounded(1);
    unsafe {
        store.getAllCookies(&block2::RcBlock::new(
            move |stored: std::ptr::NonNull<
                objc2_foundation::NSArray<objc2_foundation::NSHTTPCookie>,
            >| {
                let identity = |cookie: &objc2_foundation::NSHTTPCookie| {
                    (
                        cookie.domain().to_string(),
                        cookie.path().to_string(),
                        cookie.name().to_string(),
                    )
                };
                let stored: std::collections::HashMap<_, _> = stored
                    .as_ref()
                    .iter()
                    .map(|cookie| (identity(&cookie), cookie))
                    .collect();
                let matches = native.iter().all(|expected| {
                    stored.get(&identity(expected)).is_some_and(|actual| {
                        actual.value() == expected.value()
                            && actual.isHTTPOnly() == expected.isHTTPOnly()
                            && actual.isSecure() == expected.isSecure()
                            && actual.isSessionOnly() == expected.isSessionOnly()
                            && actual.expiresDate() == expected.expiresDate()
                            && actual.sameSitePolicy() == expected.sameSitePolicy()
                    })
                });
                let _ = tx.try_send(matches);
            },
        ));
    }
    if !rx
        .recv()
        .await
        .map_err(|_| "Can't confirm which cookies were saved.")?
    {
        return Err("Some cookies couldn't be saved with their original attributes. Others were saved. Try again.".into());
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod chrome_tests {
    use rusqlite::Connection;

    pub(super) fn fixture() -> (tempfile::TempDir, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let db = Connection::open(directory.path().join("Cookies")).unwrap();
        db.execute_batch(include_str!("../tests/fixtures/chrome-cookies.sql"))
            .unwrap();
        (directory, db)
    }
    pub(super) fn password() -> Result<Vec<u8>, String> {
        Ok(b"fixture-password".to_vec())
    }

    #[test]
    fn decrypts_chrome_v24_and_preserves_native_cookie_attributes() {
        use super::{native_chrome_cookie, read_chrome_cookies};
        let (directory, db) = fixture();
        let original = std::fs::read(directory.path().join("Cookies")).unwrap();
        let (cookies, skipped) =
            read_chrome_cookies(directory.path(), 1_700_000_000, password).unwrap();
        assert_eq!((cookies.len(), skipped), (1, 0));
        let cookie = &cookies[0];
        assert_eq!(&**cookie.value, "fixture-login");
        assert_eq!(cookie.expires, Some(1_800_000_000));
        let native = native_chrome_cookie(cookie).unwrap();
        assert_eq!(native.domain().to_string(), ".example.test");
        assert_eq!(native.name().to_string(), "login");
        assert_eq!(native.value().to_string(), "fixture-login");
        assert_eq!(native.path().to_string(), "/account");
        assert!(native.isHTTPOnly());
        assert!(native.isSecure());
        assert!(!native.isSessionOnly());
        assert_eq!(native.sameSitePolicy().unwrap().to_string(), "lax");
        assert_eq!(
            native.expiresDate().unwrap().timeIntervalSince1970(),
            1_800_000_000.0
        );
        assert_eq!(
            std::fs::read(directory.path().join("Cookies")).unwrap(),
            original
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cookies", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn reads_committed_wal_session_cookies_without_reading_keychain_or_uncommitted_rows() {
        use super::{native_chrome_cookie, read_chrome_cookies};
        let (directory, db) = fixture();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            UPDATE cookies SET value='session', encrypted_value=X'', host_key='example.test',
            has_expires=0, is_persistent=0, expires_utc=0, is_secure=0, is_httponly=0, samesite=2;
            BEGIN IMMEDIATE; UPDATE cookies SET value='uncommitted';",
        )
        .unwrap();
        let (cookies, skipped) = read_chrome_cookies(directory.path(), 1_900_000_000, || {
            panic!("plaintext needs no Keychain")
        })
        .unwrap();
        assert_eq!((cookies.len(), skipped), (1, 0));
        let native = native_chrome_cookie(&cookies[0]).unwrap();
        assert_eq!(native.value().to_string(), "session");
        assert_eq!(native.domain().to_string(), "example.test");
        assert!(!native.isHTTPOnly());
        assert!(!native.isSecure());
        assert!(native.isSessionOnly());
        assert!(native.expiresDate().is_none());
        assert_eq!(native.sameSitePolicy().unwrap().to_string(), "strict");
        db.execute_batch("ROLLBACK;").unwrap();
    }

    #[test]
    fn supports_v23_ciphertext_and_empty_values_and_samesite_none() {
        use super::{native_chrome_cookie, read_chrome_cookies};
        let (directory, db) = fixture();
        db.execute_batch("UPDATE meta SET value='23' WHERE key='version';
            UPDATE cookies SET encrypted_value=X'7631309d53da233d3900ecc985316cd65cae4a', samesite=0;").unwrap();
        let (cookies, _) = read_chrome_cookies(directory.path(), 0, password).unwrap();
        assert_eq!(&**cookies[0].value, "fixture-login");
        // Foundation represents unrestricted cookies as nil, which WebKit maps to None.
        assert!(
            native_chrome_cookie(&cookies[0])
                .unwrap()
                .sameSitePolicy()
                .is_none()
        );
        db.execute_batch("UPDATE cookies SET encrypted_value=X'', value='', samesite=-1;")
            .unwrap();
        let (cookies, _) =
            read_chrome_cookies(directory.path(), 0, || panic!("empty plaintext")).unwrap();
        assert_eq!(cookies.len(), 1);
        assert!(cookies[0].value.is_empty());
        assert_eq!(
            native_chrome_cookie(&cookies[0])
                .unwrap()
                .sameSitePolicy()
                .unwrap()
                .to_string(),
            "lax"
        );
    }

    #[test]
    fn excludes_expired_and_partitioned_cookies_without_decrypting_them() {
        use super::read_chrome_cookies;
        let (directory, db) = fixture();
        let (cookies, skipped) =
            read_chrome_cookies(directory.path(), 1_800_000_000, || panic!("expired")).unwrap();
        assert!(cookies.is_empty());
        assert_eq!(skipped, 1);
        db.execute_batch("UPDATE cookies SET top_frame_site_key='https://other.test';")
            .unwrap();
        let (cookies, skipped) =
            read_chrome_cookies(directory.path(), 0, || panic!("partitioned")).unwrap();
        assert!(cookies.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn rejects_wrong_key_domain_hash_and_unknown_format_without_exposing_values() {
        use super::read_chrome_cookies;
        let (directory, db) = fixture();
        for password in [b"wrong".to_vec(), Vec::new()] {
            let error = read_chrome_cookies(directory.path(), 0, || Ok(password))
                .err()
                .unwrap();
            assert!(!error.contains("fixture-login"));
        }
        db.execute_batch("UPDATE cookies SET host_key='.elsewhere.test';")
            .unwrap();
        assert!(
            read_chrome_cookies(directory.path(), 0, password)
                .err()
                .unwrap()
                .contains("domain check")
        );
        db.execute_batch("UPDATE cookies SET encrypted_value=X'763230001122';")
            .unwrap();
        assert!(read_chrome_cookies(directory.path(), 0, || panic!("unknown cipher")).is_err());
        db.execute_batch("UPDATE meta SET value='25' WHERE key='version';")
            .unwrap();
        assert!(read_chrome_cookies(directory.path(), 0, || panic!("unknown schema")).is_err());
    }

    #[test]
    fn can_retry_after_denied_keychain_and_missing_database() {
        use super::read_chrome_cookies;
        let directory = tempfile::tempdir().unwrap();
        assert!(read_chrome_cookies(directory.path(), 0, password).is_err());
        assert!(!directory.path().join("Cookies").exists());
        let db = Connection::open(directory.path().join("Cookies")).unwrap();
        db.execute_batch(include_str!("../tests/fixtures/chrome-cookies.sql"))
            .unwrap();
        assert_eq!(
            read_chrome_cookies(directory.path(), 0, || Err("denied".into()))
                .err()
                .unwrap(),
            "denied"
        );
        let (cookies, skipped) = read_chrome_cookies(directory.path(), 0, password).unwrap();
        assert_eq!((cookies.len(), skipped), (1, 0));
    }

    #[test]
    fn lists_named_profiles_with_both_cookie_locations_and_rejects_path_traversal() {
        use super::chrome_profiles;
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("Default/Network")).unwrap();
        std::fs::create_dir_all(directory.path().join("Profile 2")).unwrap();
        std::fs::write(directory.path().join("Default/Network/Cookies"), []).unwrap();
        std::fs::write(directory.path().join("Profile 2/Cookies"), []).unwrap();
        std::fs::write(directory.path().join("Local State"), r#"{"profile":{"info_cache":{"Default":{"name":"Personal"},"Profile 2":{"name":"Work"},"Missing":{"name":"removed"},"../escape":{"name":"invalid"}}}}"#).unwrap();
        let profiles = chrome_profiles(directory.path()).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            profiles[0],
            (
                "Personal (Default)".into(),
                directory.path().join("Default")
            )
        );
        assert_eq!(
            profiles[1],
            (
                "Work (Profile 2)".into(),
                directory.path().join("Profile 2")
            )
        );
        std::fs::write(directory.path().join("Local State"), "broken").unwrap();
        assert!(chrome_profiles(directory.path()).is_err());
    }
}
