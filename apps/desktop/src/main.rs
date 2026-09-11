mod app;
mod browser;
mod diff;
mod platform;
mod store_session;
mod terminal;
use futures_util::FutureExt;
use gpui_kit::{
    component::{Root, Theme, ThemeMode},
    *,
};
#[derive(Clone)]
pub(crate) struct Runtime {
    pub(crate) handle: tokio::runtime::Handle,
    pub(crate) connections: std::sync::Arc<platform::Connections>,
    pub(crate) closing: tokio_util::task::TaskTracker,
    pub(crate) logging_error: Option<String>,
}
impl Global for Runtime {}
struct DesktopAssets;
impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "bex/microphone.svg" => include_bytes!("../assets/microphone.svg"),
            "bex/gauge.svg" => include_bytes!("../assets/gauge.svg"),
            "bex/stop.svg" => include_bytes!("../assets/stop.svg"),
            _ => return gpui_kit::assets::Assets.load(path),
        };
        Ok(Some(std::borrow::Cow::Borrowed(bytes)))
    }
    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(
            ["bex/microphone.svg", "bex/gauge.svg", "bex/stop.svg"]
                .into_iter()
                .filter(|item| item.starts_with(path))
                .map(SharedString::from),
        );
        Ok(paths)
    }
}
fn main() {
    let logging_error = platform::state_dir()
        .and_then(|directory| {
            agent_core::diagnostics::initialize(
                &directory,
                agent_core::diagnostics::Component::Desktop,
                env!("CARGO_PKG_VERSION"),
            )
            .map_err(|error| error.to_string())
        })
        .err()
        .map(|error| format!("エラーログを保存できません: {error}"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("start async runtime");
    let handle = runtime.handle().clone();
    let closing = tokio_util::task::TaskTracker::new();
    let shutdown = closing.clone();
    gpui_kit::application()
        .with_assets(DesktopAssets)
        .with_http_client(std::sync::Arc::new(gpui_http::ReqwestClient::new()))
        .run(move |cx| {
            cx.set_global(Runtime {
                handle,
                closing,
                logging_error,
                connections: std::sync::Arc::new(platform::Connections::default()),
            });
            cx.on_app_quit(|cx| {
                agent_core::diagnostics::shutdown();
                let runtime = cx.global::<Runtime>().clone();
                async move {
                    runtime.closing.close();
                    let handle = runtime.handle.clone();
                    let _ = handle
                        .spawn(async move {
                            runtime.closing.wait().await;
                            runtime.connections.close().await;
                        })
                        .await;
                }
                .boxed_local()
            })
            .detach();
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
    shutdown.close();
    runtime.block_on(shutdown.wait());
}
