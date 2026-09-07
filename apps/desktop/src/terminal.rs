use crate::{
    conversation::text,
    platform,
    rpc::{self, Rpc},
};
use base64::Engine;
use gpui_kit::{
    component::{h_flex, v_flex},
    *,
};
use gpui_wry::WebView;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

enum Event {
    Frontend(Value),
    Rpc(rpc::Event),
    Reply(Result<Value, String>),
}

/// One PTY on the selected Host. Panel visibility does not restart the shell.
pub(crate) struct Terminal {
    requests: std::sync::mpsc::Sender<(&'static str, Value)>,
    webview: Entity<WebView>,
    handle: String,
    cwd: String,
    online: bool,
    ready: bool,
    started: bool,
    exited: bool,
    cols: u16,
    rows: u16,
    status: String,
}
impl Terminal {
    pub(crate) fn new(
        remote: &str,
        cwd: String,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, String> {
        let html = terminal_html()?;
        let (tx, rx) = async_channel::unbounded();
        let frontend = tx.clone();
        let raw = wry::WebViewBuilder::new()
            .with_html(html)
            .with_background_color((24, 24, 24, 255))
            .with_focused(false)
            .with_accept_first_mouse(true)
            .with_navigation_handler(|url| url == "about:blank")
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_ipc_handler(move |request| {
                if let Ok(value) = serde_json::from_str::<Value>(request.body()) {
                    let _ = frontend.try_send(Event::Frontend(value));
                }
            })
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        let webview = cx.new(|cx| WebView::new(raw, window, cx));
        let events = tx.clone();
        let target = if remote.is_empty() {
            json!({"target":"local"})
        } else {
            json!({"target":"remote","profileId":remote})
        };
        let rpc = Rpc::connect(
            platform::state_dir().join("host.sock"),
            target,
            move |event| {
                let _ = events.send_blocking(Event::Rpc(event));
            },
        );
        let handle = format!("bex-terminal-{}", uuid::Uuid::new_v4());
        let process_handle = handle.clone();
        let (requests, commands) = std::sync::mpsc::channel::<(&'static str, Value)>();
        // Serialize input and resize requests so scheduler order cannot reorder shell bytes.
        std::thread::spawn(move || {
            let mut started = false;
            for (method, params) in commands {
                started |= method == "process/spawn";
                let result = rpc.request(method, params);
                let _ = tx.send_blocking(Event::Reply(result));
            }
            if started {
                let _ = rpc.request("process/kill", json!({"processHandle":process_handle}));
            }
            rpc.close();
        });
        Ok(cx.new(|cx: &mut Context<Self>| {
            cx.spawn_in(window, async move |view, cx| {
                while let Ok(event) = rx.recv().await {
                    if view.update_in(cx, |s, _, cx| s.event(event, cx)).is_err() {
                        break;
                    }
                }
            })
            .detach();
            Self {
                requests,
                webview,
                handle,
                cwd,
                online: false,
                ready: false,
                started: false,
                exited: false,
                cols: 80,
                rows: 24,
                status: "接続中…".into(),
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
    fn request(&self, method: &'static str, params: Value) {
        let _ = self.requests.send((method, params));
    }

    fn script(&self, script: String, cx: &App) {
        let _ = self.webview.read(cx).raw().evaluate_script(&script);
    }
    fn notice(&self, message: &str, cx: &App) {
        self.script(format!("window.bexTerminal.status({})", json!(message)), cx);
    }
    fn event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Frontend(value) => match text(&value, "type") {
                "ready" => {
                    self.ready = true;
                    self.resize(&value);
                }
                "resize" => {
                    self.resize(&value);
                    if self.started && self.online && !self.exited {
                        self.request("process/resizePty", json!({"processHandle":self.handle,"size":{"cols":self.cols,"rows":self.rows}}));
                    }
                }
                "input" if self.started && self.online && !self.exited => {
                    if let Some(data) = value["data"].as_str() {
                        for chunk in data.as_bytes().chunks(16 * 1024) {
                            self.request("process/writeStdin", json!({"processHandle":self.handle,"deltaBase64":base64::engine::general_purpose::STANDARD.encode(chunk)}));
                        }
                    }
                }
                "paste" => {
                    if let Some(data) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.script(format!("window.bexTerminal.paste({})", json!(data)), cx);
                    }
                }
                "copy" => {
                    if let Some(data) = value["data"].as_str() {
                        cx.write_to_clipboard(ClipboardItem::new_string(data.to_owned()));
                    }
                }
                _ => {}
            },
            Event::Rpc(rpc::Event::Connected(online, reason)) => {
                self.online = online;
                if !online {
                    self.status = reason;
                    if self.started {
                        // A reconnect cannot recover missed screen bytes. Require an explicit new shell.
                        self.exited = true;
                        if self.ready {
                            self.notice("接続が切れました。新しいターミナルを開いてください。", cx);
                        }
                    }
                }
            }
            Event::Rpc(rpc::Event::Message(value)) => {
                let params = &value["params"];
                if params["processHandle"] != self.handle {
                    return;
                }
                match text(&value, "method") {
                    "process/outputDelta" => {
                        self.status = "実行中".into();
                        if let Some(delta) = params["deltaBase64"].as_str() {
                            self.script(format!("window.bexTerminal.write({})", json!(delta)), cx);
                        }
                        if params["capReached"] == true {
                            self.notice("出力が Host の上限に達しました。", cx);
                        }
                    }
                    "process/exited" => {
                        self.exited = true;
                        self.status = format!("終了 · {}", params["exitCode"]);
                        self.notice(&self.status, cx);
                    }
                    _ => {}
                }
            }
            Event::Reply(Err(error)) => {
                self.status = error;
                if self.ready {
                    self.notice(&self.status, cx);
                }
            }
            Event::Reply(Ok(_)) => {}
        }
        if self.online && self.ready && !self.started {
            self.started = true;
            self.status = "接続中".into();
            self.request("process/spawn", json!({
                "processHandle":self.handle,"cwd":self.cwd,
                "command":["/bin/sh","-c","exec \"${SHELL:-/bin/sh}\" -l"],
                "env":{"TERM":"xterm-256color","COLORTERM":"truecolor"},
                "tty":true,"streamStdin":true,"streamStdoutStderr":true,
                "timeoutMs":null,"outputBytesCap":null,"size":{"cols":self.cols,"rows":self.rows}
            }));
        }
        cx.notify();
    }
    fn resize(&mut self, value: &Value) {
        self.cols = value["cols"].as_u64().unwrap_or(80).clamp(2, 500) as u16;
        self.rows = value["rows"].as_u64().unwrap_or(24).clamp(1, 250) as u16;
    }
}
impl Render for Terminal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
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
                    .child(self.status.clone()),
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
