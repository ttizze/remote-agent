mod app;
mod browser;
mod conversation;
mod diff;
mod platform;
mod rpc;
mod terminal;
use gpui_kit::{
    component::{Root, Theme, ThemeMode},
    *,
};
fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_http_client(std::sync::Arc::new(gpui_http::ReqwestClient::new()))
        .run(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys([KeyBinding::new(
                "ctrl-v",
                gpui_kit::component::input::Paste,
                Some("ChatComposer > Input"),
            )]);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1440.), px(960.)), cx);
            cx.spawn(async move |cx| {
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Bex".into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(14.), px(14.))),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    |window, cx| {
                        let desktop = cx.new(|cx| app::Desktop::new(app::Mode::Main, window, cx));
                        cx.new(|cx| Root::new(desktop, window, cx))
                    },
                )
                .expect("open Bex window");
            })
            .detach();
            cx.activate(true);
        });
}
