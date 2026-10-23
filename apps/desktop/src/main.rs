mod app;
mod appearance;
mod browser;
mod diff;
mod platform;
mod store_session;
mod terminal;
use futures_util::FutureExt;
use gpui_kit::{component::Root, *};
const WINDOW_HEADER_HEIGHT: f32 = 44.;
pub(crate) const APP_NAME: &str = env!("BEX_APP_NAME");
#[derive(Clone)]
pub(crate) struct Runtime {
    pub(crate) handle: tokio::runtime::Handle,
    pub(crate) connections: std::sync::Arc<platform::Connections>,
    pub(crate) closing: tokio_util::task::TaskTracker,
    pub(crate) logging_error: Option<String>,
}
impl Global for Runtime {}
struct DesktopAssets;
impl DesktopAssets {
    const FILES: &[(&str, &[u8])] = &[
        (
            "bex/openai.svg",
            include_bytes!("../../mobile/iosApp/Bex/Assets.xcassets/openai.imageset/openai.svg"),
        ),
        (
            "bex/anthropic.svg",
            include_bytes!(
                "../../mobile/iosApp/Bex/Assets.xcassets/anthropic.imageset/anthropic.svg"
            ),
        ),
        ("bex/deepseek.svg", include_bytes!("../assets/deepseek.svg")),
        (
            "bex/claude.svg",
            include_bytes!("../../mobile/iosApp/Bex/Assets.xcassets/claude.imageset/claude.svg"),
        ),
        ("bex/shield.svg", include_bytes!("../assets/shield.svg")),
        ("bex/bolt.svg", include_bytes!("../assets/bolt.svg")),
        (
            "bex/bolt-fill.svg",
            include_bytes!("../assets/bolt-fill.svg"),
        ),
        (
            "bex/microphone.svg",
            include_bytes!("../assets/microphone.svg"),
        ),
        ("bex/stop.svg", include_bytes!("../assets/stop.svg")),
        ("bex/pencil.svg", include_bytes!("../assets/pencil.svg")),
        ("bex/download.svg", include_bytes!("../assets/download.svg")),
        (
            "bex/square-pen.svg",
            include_bytes!("../assets/square-pen.svg"),
        ),
        (
            "bex/folder-plus.svg",
            include_bytes!("../assets/folder-plus.svg"),
        ),
        ("bex/branch.svg", include_bytes!("../assets/branch.svg")),
        ("bex/merge.svg", include_bytes!("../assets/merge.svg")),
        ("bex/diff.svg", include_bytes!("../assets/diff.svg")),
        ("bex/monitor.svg", include_bytes!("../assets/monitor.svg")),
        ("bex/qr-code.svg", include_bytes!("../assets/qr-code.svg")),
    ];
}
impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = Self::FILES.iter().find(|(name, _)| *name == path) {
            Ok(Some(std::borrow::Cow::Borrowed(*bytes)))
        } else {
            gpui_kit::assets::Assets.load(path)
        }
    }
    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(
            Self::FILES
                .iter()
                .map(|(name, _)| *name)
                .filter(|item| item.starts_with(path))
                .map(SharedString::from),
        );
        Ok(paths)
    }
}

struct MainWindow(Option<WindowHandle<Root>>);
impl Global for MainWindow {}

fn open_main_window(cx: &mut App) {
    if let Some(window) = cx.global::<MainWindow>().0
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }
    let bounds = Bounds::centered(None, size(px(1440.), px(960.)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Maximized(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    appears_transparent: !cfg!(target_os = "windows"),
                    traffic_light_position: Some(point(
                        px(14.),
                        px((WINDOW_HEADER_HEIGHT - 14.) / 2.),
                    )),
                }),
                ..Default::default()
            },
            |window, cx| {
                #[cfg(target_os = "windows")]
                appearance::style_native_titlebar(window);
                let desktop = cx.new(|cx| app::Desktop::new(app::Mode::Main, window, cx));
                cx.new(|cx| Root::new(desktop, window, cx))
            },
        )
        .expect("open Bex window");
    cx.global_mut::<MainWindow>().0 = Some(window);
    cx.activate(true);
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--build-info") {
        println!(
            "{}",
            serde_json::json!({
                "profile": env!("BEX_BUILD_PROFILE"),
                "appName": APP_NAME,
                "debugAssertions": cfg!(debug_assertions),
                "revision": option_env!("BEX_BUILD_REVISION").unwrap_or("development"),
            })
        );
        return;
    }
    // GPUI's DirectComposition target is above child HWNDs, hiding WebView2
    // behind the app surface. Use its HWND swap-chain renderer for embedding.
    // Set this before any runtime or platform threads can read the environment.
    #[cfg(target_os = "windows")]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1");
    }
    #[cfg(target_os = "macos")]
    let _ = std::thread::Builder::new()
        .name("microphone-prepare".into())
        .spawn(platform::prepare_microphone);
    let logging_error = platform::state_dir()
        .and_then(|directory| {
            agent_transport::diagnostics::initialize(
                &directory,
                agent_transport::diagnostics::Component::Desktop,
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
    let application = gpui_kit::application()
        .with_assets(DesktopAssets)
        .with_http_client(std::sync::Arc::new(gpui_http::ReqwestClient::new()));
    application.on_reopen(open_main_window);
    application.run(move |cx| {
        cx.set_global(Runtime {
            handle,
            closing,
            logging_error,
            connections: std::sync::Arc::new(platform::Connections::default()),
        });
        cx.on_app_quit(|cx| {
            tracing::info!(target: "bex", operation = "shutdown", "Bex shutting down");
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
        appearance::init(cx);
        cx.bind_keys([KeyBinding::new(
            "ctrl-v",
            gpui_kit::component::input::Paste,
            Some("ChatComposer > Input"),
        )]);
        cx.set_global(MainWindow(None));
        #[cfg(target_os = "macos")]
        platform::task_menu::init(cx);
        cx.spawn(async move |cx| {
            cx.update(open_main_window);
        })
        .detach();
        cx.activate(true);
    });
    shutdown.close();
    runtime.block_on(shutdown.wait());
}

#[cfg(test)]
mod asset_tests {
    use super::DesktopAssets;
    use gpui_kit::{
        AssetSource,
        component::{IconName, IconNamed},
    };
    use std::borrow::Cow;

    #[test]
    fn standard_icons_are_embedded_in_development_bundles() {
        for icon in [
            IconName::Search,
            IconName::Settings,
            IconName::Plus,
            IconName::Folder,
            IconName::PanelLeftClose,
            IconName::PanelRightOpen,
            IconName::ArrowUp,
        ] {
            let path = icon.path();
            let bytes = DesktopAssets.load(&path).unwrap().unwrap();
            assert!(
                matches!(bytes, Cow::Borrowed(_)),
                "{path} must not require the build machine's files"
            );
            assert!(!bytes.is_empty());
        }
    }
}
