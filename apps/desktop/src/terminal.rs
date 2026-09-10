use crate::Runtime;
use agent_core::state::operations as op;
use agent_core::{
    client::TerminalSize,
    state::{Intent, Snapshot, TerminalPhase},
    store::Store,
};
use gpui_kit::{
    component::{h_flex, v_flex},
    *,
};
use gpui_wry::WebView;
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Frontend {
    Ready { cols: u16, rows: u16 },
    Resize { cols: u16, rows: u16 },
    Input { data: String },
    Copy { data: String },
    Paste,
    Acknowledge { sequence: u64 },
}
enum Event {
    Frontend(Frontend),
    Connected(Result<Arc<Store>, String>),
    Snapshot(Arc<Snapshot>),
    Error(String),
}

/// A terminal view owns one Store and one iroh session. Output remains in the
/// immutable snapshot until xterm confirms it has consumed the corresponding bytes.
pub(crate) struct Terminal {
    store: Option<Arc<Store>>,
    snapshot: Arc<Snapshot>,
    events: async_channel::Sender<Event>,
    runtime: Runtime,
    webview: Entity<WebView>,
    handle: String,
    cwd: String,
    ready: bool,
    start_requested: bool,
    sent_sequence: u64,
    size: TerminalSize,
    sent_size: Option<TerminalSize>,
    error: Option<String>,
    noticed: Option<String>,
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}
impl Terminal {
    fn close(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let store = self.store.take()?;
        Some(self.runtime.closing.spawn_on(
            async move {
                let _ = store.close().await;
            },
            &self.runtime.handle,
        ))
    }
    pub(crate) fn new(
        remote: &str,
        cwd: String,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, String> {
        let html = terminal_html()?;
        let (events, incoming) = async_channel::unbounded();
        let frontend = events.clone();
        let raw = wry::WebViewBuilder::new()
            .with_html(html)
            .with_background_color((24, 24, 24, 255))
            .with_focused(false)
            .with_accept_first_mouse(true)
            .with_navigation_handler(|url| url == "about:blank")
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_ipc_handler(move |request| {
                if let Ok(value) = serde_json::from_str(request.body()) {
                    let _ = frontend.try_send(Event::Frontend(value));
                }
            })
            .build_as_child(window)
            .map_err(|error| error.to_string())?;
        let webview = cx.new(|cx| WebView::new(raw, window, cx));
        let runtime = cx.global::<Runtime>().clone();
        let remote = (!remote.is_empty()).then(|| remote.to_owned());
        let updates = events.clone();
        let connections = runtime.connections.clone();
        runtime.handle.spawn(async move {
            match connections
                .connect(remote.as_deref(), Snapshot::default())
                .await
            {
                Ok(store) => {
                    let mut snapshots = store.subscribe();
                    if updates
                        .send(Event::Connected(Ok(Arc::new(store))))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    loop {
                        let snapshot = snapshots.borrow_and_update().clone();
                        if updates.send(Event::Snapshot(snapshot)).await.is_err() {
                            break;
                        }
                        if snapshots.changed().await.is_err() {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = updates.send(Event::Connected(Err(error))).await;
                }
            }
        });
        Ok(cx.new(|cx: &mut Context<Self>| {
            cx.on_app_quit(|view, _| {
                if let Some(close) = view.close() {
                    let _ = view.runtime.handle.block_on(close);
                }
                async {}
            })
            .detach();
            cx.spawn_in(window, async move |view, cx| {
                while let Ok(event) = incoming.recv().await {
                    if view
                        .update_in(cx, |view, _, cx| view.event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Self {
                store: None,
                snapshot: Arc::default(),
                events,
                runtime,
                webview,
                handle: format!("bex-terminal-{}", uuid::Uuid::new_v4()),
                cwd,
                ready: false,
                start_requested: false,
                sent_sequence: 0,
                size: TerminalSize { cols: 80, rows: 24 },
                sent_size: None,
                error: None,
                noticed: None,
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
    fn dispatch(&self, intent: Intent) {
        if let Some(store) = &self.store {
            let receipt = store.dispatch(intent);
            let events = self.events.clone();
            self.runtime.handle.spawn(async move {
                if let Err(error) = receipt.await {
                    let _ = events.send(Event::Error(error.to_string())).await;
                }
            });
        }
    }
    fn script(&self, script: String, cx: &App) {
        let _ = self.webview.read(cx).raw().evaluate_script(&script);
    }
    fn event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Connected(Ok(store)) => {
                self.snapshot = store.snapshot();
                self.store = Some(store);
            }
            Event::Connected(Err(error)) | Event::Error(error) => self.error = Some(error),
            Event::Snapshot(snapshot) => self.snapshot = snapshot,
            Event::Frontend(Frontend::Ready { cols, rows }) => {
                self.ready = true;
                self.size = TerminalSize {
                    cols: cols.clamp(2, 500),
                    rows: rows.clamp(1, 250),
                };
            }
            Event::Frontend(Frontend::Resize { cols, rows }) => {
                self.size = TerminalSize {
                    cols: cols.clamp(2, 500),
                    rows: rows.clamp(1, 250),
                };
            }
            Event::Frontend(Frontend::Input { data }) => {
                if self
                    .snapshot
                    .terminals
                    .get(&self.handle)
                    .is_some_and(|terminal| terminal.phase == TerminalPhase::Running)
                {
                    self.dispatch(Intent::WriteTerminal(op::WriteTerminal {
                        handle: self.handle.clone(),
                        data: data.into_bytes(),
                    }));
                }
            }
            Event::Frontend(Frontend::Acknowledge { sequence }) => {
                self.dispatch(Intent::AcknowledgeTerminal {
                    handle: self.handle.clone(),
                    sequence,
                })
            }
            Event::Frontend(Frontend::Paste) => {
                if let Some(data) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.script(format!("window.bexTerminal.paste({})", json!(data)), cx);
                }
            }
            Event::Frontend(Frontend::Copy { data }) => {
                cx.write_to_clipboard(ClipboardItem::new_string(data))
            }
        }
        if self.ready && self.snapshot.connected && !self.start_requested {
            // The handle is dispatched once at readiness; the next UI event may
            // precede the asynchronous Store publication.
            self.start_requested = true;
            self.sent_size = Some(self.size);
            self.dispatch(Intent::StartTerminal(op::StartTerminal {
                handle: self.handle.clone(),
                cwd: self.cwd.clone(),
                size: self.size,
            }));
        }
        if self.sent_size != Some(self.size)
            && self
                .snapshot
                .terminals
                .get(&self.handle)
                .is_some_and(|terminal| terminal.phase == TerminalPhase::Running)
        {
            self.sent_size = Some(self.size);
            self.dispatch(Intent::ResizeTerminal(op::ResizeTerminal {
                handle: self.handle.clone(),
                size: self.size,
            }));
        }
        if self.ready {
            if let Some(terminal) = self.snapshot.terminals.get(&self.handle) {
                for chunk in &terminal.output {
                    if chunk.sequence > self.sent_sequence {
                        self.script(
                            format!(
                                "window.bexTerminal.write({}, {})",
                                json!(chunk.data),
                                chunk.sequence
                            ),
                            cx,
                        );
                        if chunk.cap_reached {
                            self.script(
                                format!(
                                    "window.bexTerminal.status({})",
                                    json!("出力が Host の上限に達しました。")
                                ),
                                cx,
                            );
                        }
                        self.sent_sequence = chunk.sequence;
                    }
                }
            }
            let notice = self.error.clone().or_else(|| {
                self.snapshot
                    .terminals
                    .get(&self.handle)
                    .and_then(|terminal| match &terminal.phase {
                        TerminalPhase::Exited(code) => Some(format!("終了 · {code}")),
                        TerminalPhase::Failed(error) => Some(error.clone()),
                        _ => None,
                    })
            });
            if notice != self.noticed {
                if let Some(notice) = &notice {
                    self.script(format!("window.bexTerminal.status({})", json!(notice)), cx);
                }
                self.noticed = notice;
            }
        }
        cx.notify();
    }
}
impl Render for Terminal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let status = self.error.clone().unwrap_or_else(|| {
            self.snapshot
                .terminals
                .get(&self.handle)
                .map_or("接続中…".into(), |terminal| match &terminal.phase {
                    TerminalPhase::Starting => "起動中…".into(),
                    TerminalPhase::Running => "実行中".into(),
                    TerminalPhase::Exited(code) => format!("終了 · {code}"),
                    TerminalPhase::Failed(error) => error.clone(),
                    TerminalPhase::Closed => "終了".into(),
                })
        });
        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(rgb(0x999999))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .child(self.cwd.clone()),
                    )
                    .child(status),
            )
            .child(div().flex_1().min_h_0().pl_1().child(self.webview.clone()))
    }
}

fn terminal_html() -> Result<String, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundled = executable
        .parent()
        .unwrap_or(Path::new("."))
        .join("../Resources/terminal");
    let assets: [(PathBuf, &str); 3] = [
        (PathBuf::from("@xterm/xterm/lib/xterm.js"), "/*XTERM_JS*/"),
        (PathBuf::from("@xterm/xterm/css/xterm.css"), "/*XTERM_CSS*/"),
        (
            PathBuf::from("@xterm/addon-fit/lib/addon-fit.js"),
            "/*FIT_JS*/",
        ),
    ];
    let mut html = include_str!("../web/terminal.html").to_owned();
    for (path, marker) in assets {
        let source = if bundled.is_dir() {
            bundled.join(path.file_name().unwrap())
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("web/node_modules")
                .join(path)
        };
        let content = std::fs::read_to_string(source).map_err(|e| format!("ターミナル部品を読み込めません。npm --prefix apps/desktop/web ci を実行してください: {e}"))?;
        html = html.replace(marker, &content.replace("</script", "<\\/script"));
    }
    Ok(html)
}
