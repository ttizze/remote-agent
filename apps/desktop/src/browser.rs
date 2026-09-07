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

enum Event {
    Location(String),
    Navigate(String),
}

pub(crate) struct Browser {
    webview: Entity<WebView>,
    address: Entity<InputState>,
    error: String,
    _subscription: Subscription,
}

impl Browser {
    pub(crate) fn new(window: &mut Window, cx: &mut App) -> Result<Entity<Self>, String> {
        let (tx, rx) = async_channel::unbounded();
        let navigation = tx.clone();
        let raw = wry::WebViewBuilder::new()
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
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("URL を入力"));
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
                _subscription: subscription,
            }
        }))
    }
    pub(crate) fn set_visible(&self, visible: bool, cx: &mut App) {
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
}

fn browser_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input == "about:blank" {
        return Ok(input.into());
    }
    if input.is_empty() {
        return Err("URL を入力してください".into());
    }
    let source = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&source).map_err(|_| "URL が不正です")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("http または https の URL を入力してください".into());
    }
    Ok(url.into())
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
                            .tooltip("戻る")
                            .accessibility_label("戻る")
                            .on_click(cx.listener(|s, _, _, cx| {
                                let _ = s.webview.read(cx).raw().evaluate_script("history.back()");
                            })),
                    )
                    .child(
                        Button::new("browser-forward")
                            .icon(IconName::ArrowRight)
                            .small()
                            .ghost()
                            .tooltip("進む")
                            .accessibility_label("進む")
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
                            .tooltip("再読み込み")
                            .accessibility_label("再読み込み")
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
                            .aria_label("ブラウザの URL"),
                    )
                    .child(
                        Button::new("browser-go")
                            .icon(IconName::ArrowRight)
                            .small()
                            .ghost()
                            .tooltip("URL を開く")
                            .accessibility_label("URL を開く")
                            .on_click(cx.listener(|s, _, _, cx| s.navigate(cx))),
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
            .child(div().flex_1().min_h_0().pl_1().child(self.webview.clone()))
    }
}
