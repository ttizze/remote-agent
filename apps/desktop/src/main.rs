mod app;
mod browser;
mod diff;
mod platform;
mod store_session;
mod terminal;
use futures_util::FutureExt;
use gpui_kit::{component::Root, *};
const WINDOW_HEADER_HEIGHT: f32 = 52.;
#[derive(Clone)]
pub(crate) struct Runtime {
    pub(crate) handle: tokio::runtime::Handle,
    pub(crate) connections: std::sync::Arc<platform::Connections>,
    pub(crate) closing: tokio_util::task::TaskTracker,
    pub(crate) logging_error: Option<String>,
}
impl Global for Runtime {}
struct DesktopAssets;
/// Lucide icons as `lucide/<name>.svg`.
macro_rules! lucide {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("lucide/", $name, ".svg"),
            include_bytes!(concat!("../assets/lucide/", $name, ".svg")) as &[u8],
        )),*]
    };
}
impl DesktopAssets {
    const ICONS: &[(&str, &[u8])] = lucide![
        "alarm-clock-off",
        "alarm-clock",
        "archive",
        "arrow-down",
        "arrow-left",
        "arrow-up",
        "book-open",
        "bot",
        "brain",
        "check",
        "chevron-down",
        "chevron-left",
        "chevron-right",
        "chevron-up",
        "circle-alert",
        "circle-check",
        "circle-dashed",
        "circle-dot",
        "circle-x",
        "clock",
        "columns-2",
        "copy",
        "corner-up-right",
        "cpu",
        "download",
        "ellipsis",
        "external-link",
        "eye-off",
        "eye",
        "file-diff",
        "file-text",
        "file",
        "folder-closed",
        "folder-open",
        "folder-plus",
        "folder",
        "gauge",
        "git-branch",
        "git-compare",
        "git-fork",
        "git-merge",
        "globe",
        "grip-vertical",
        "hammer",
        "hard-drive",
        "history",
        "image",
        "info",
        "keyboard",
        "laptop",
        "layers",
        "link-2",
        "list-ordered",
        "list-plus",
        "list-todo",
        "loader-circle",
        "lock-open",
        "lock",
        "maximize-2",
        "message-circle-question",
        "message-square-dashed",
        "message-square",
        "mic",
        "minimize-2",
        "minus",
        "monitor",
        "panel-bottom",
        "panel-left",
        "panel-right",
        "paperclip",
        "pen-line",
        "pencil-ruler",
        "pencil",
        "pin-off",
        "pin",
        "play",
        "plus",
        "qr-code",
        "quote",
        "redo-2",
        "refresh-cw",
        "rotate-ccw",
        "rows-3",
        "search",
        "server",
        "settings",
        "shield-question",
        "shield",
        "sparkles",
        "square-pen",
        "square-split-horizontal",
        "square-split-vertical",
        "square-terminal",
        "square",
        "star",
        "terminal",
        "text-wrap",
        "trash-2",
        "triangle-alert",
        "undo-2",
        "unplug",
        "users",
        "wrench",
        "x",
        "zap",
    ];
    /// Provider marks as `brand/<name>.svg`.
    const BRANDS: &[(&str, &[u8])] = &[
        (
            "brand/openai.svg",
            include_bytes!("../../mobile/iosApp/Bex/Assets.xcassets/openai.imageset/openai.svg"),
        ),
        (
            "brand/anthropic.svg",
            include_bytes!(
                "../../mobile/iosApp/Bex/Assets.xcassets/anthropic.imageset/anthropic.svg"
            ),
        ),
        (
            "brand/claude.svg",
            include_bytes!("../../mobile/iosApp/Bex/Assets.xcassets/claude.imageset/claude.svg"),
        ),
    ];
    fn files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
        Self::ICONS.iter().chain(Self::BRANDS)
    }
}
impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = Self::files().find(|(name, _)| *name == path) {
            Ok(Some(std::borrow::Cow::Borrowed(*bytes)))
        } else {
            gpui_kit::assets::Assets.load(path)
        }
    }
    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(
            Self::files()
                .map(|(name, _)| *name)
                .filter(|item| item.starts_with(path))
                .map(SharedString::from),
        );
        Ok(paths)
    }
}
fn main() {
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
        .map(|error| format!("Error logs cannot be saved: {error}"));
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
                tracing::info!(target: "desktop", operation = "shutdown", "Desktop shutting down");
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
            app::bind_keys(cx);
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
                            traffic_light_position: Some(point(
                                px(14.),
                                px((WINDOW_HEADER_HEIGHT - 14.) / 2.),
                            )),
                        }),
                        ..Default::default()
                    },
                    |window, cx| {
                        app::apply_appearance(window.appearance(), cx);
                        let desktop = cx.new(|cx| app::Desktop::new(window, cx));
                        cx.new(|cx| Root::new(desktop, window, cx))
                    },
                )
                .expect("open the main window");
            })
            .detach();
            cx.activate(true);
        });
    shutdown.close();
    runtime.block_on(shutdown.wait());
}
