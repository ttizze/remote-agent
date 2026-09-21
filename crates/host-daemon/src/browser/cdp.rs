use agent_core::browser::{BrowserAction, BrowserDialog, BrowserKey, HEIGHT, WIDTH};
use async_tungstenite::{WebSocketStream, tokio::ConnectStream, tungstenite::Message};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::process::Child;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Target {
    pub target_id: String,
    pub title: String,
    pub url: String,
    pub opener_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
}

pub(super) struct Chrome {
    child: Child,
    socket: WebSocketStream<ConnectStream>,
    next_id: u64,
    sessions: HashMap<String, String>,
    dialogs: HashMap<String, BrowserDialog>,
    pub broken: bool,
}

impl Chrome {
    pub async fn launch(profile: &Path, executable: &Path) -> Result<Self, String> {
        crate::platform::create_state_directory(profile).map_err(|e| e.to_string())?;
        let port_file = profile.join("DevToolsActivePort");
        match tokio::fs::remove_file(&port_file).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let mut child = bex_process::command(executable)
            .map_err(|e| e.to_string())?
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--headless=new",
                "--remote-debugging-address=127.0.0.1",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--window-size=1024,768",
                "about:blank",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("BEXブラウザを起動できません: {e}"))?;
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    return Err("BEXブラウザが起動中に終了しました。".into());
                }
                if let Ok(contents) = tokio::fs::read_to_string(&port_file).await {
                    let mut lines = contents.lines();
                    if let (Some(port), Some(path)) = (
                        lines.next().and_then(|p| p.parse::<u16>().ok()),
                        lines.next(),
                    ) && port != 0
                        && path.starts_with("/devtools/browser/")
                        && !path.contains(['\r', '\n', '?', '#'])
                    {
                        let url = format!("ws://127.0.0.1:{port}{path}");
                        let (socket, _) = async_tungstenite::tokio::connect_async(url)
                            .await
                            .map_err(|_| "BEXブラウザに接続できません。".to_owned())?;
                        return Ok(socket);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| Err("BEXブラウザの起動がタイムアウトしました。".into()));
        match result {
            Ok(socket) => Ok(Self {
                child,
                socket,
                next_id: 0,
                sessions: HashMap::new(),
                dialogs: HashMap::new(),
                broken: false,
            }),
            Err(error) => {
                child.stdin.take();
                let _ = child.wait().await;
                Err(error)
            }
        }
    }

    pub async fn shutdown(mut self) {
        let _ = self.call(None, "Browser.close", json!({})).await;
        self.child.stdin.take();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }

    pub async fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        if self.broken {
            return Err("BEXブラウザとの接続が切れました。".into());
        }
        self.next_id += 1;
        let id = self.next_id;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = session.into();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            self.socket
                .send(Message::Text(request.to_string().into()))
                .await
                .map_err(|_| ())?;
            while let Some(message) = self.socket.next().await {
                let message = message.map_err(|_| ())?;
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
                    if value["method"] == "Page.javascriptDialogOpening" {
                        if let Some(session) = value["sessionId"].as_str() {
                            self.dialogs.insert(
                                session.into(),
                                BrowserDialog {
                                    message: value["params"]["message"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .chars()
                                        .take(4096)
                                        .collect(),
                                    prompt: value["params"]["type"] == "prompt",
                                },
                            );
                        }
                    } else if value["method"] == "Page.javascriptDialogClosed" {
                        if let Some(session) = value["sessionId"].as_str() {
                            self.dialogs.remove(session);
                        }
                    } else if value["id"] == id {
                        return Ok(value);
                    }
                } else if matches!(message, Message::Close(_)) {
                    return Err(());
                }
            }
            Err(())
        })
        .await;
        match result {
            Ok(Ok(value)) if value.get("error").is_none() => Ok(value["result"].clone()),
            Ok(Ok(_)) => Err(format!(
                "ブラウザ操作を完了できません（{method}）。画面を確認して再試行してください。"
            )),
            _ => {
                self.broken = true;
                Err("BEXブラウザとの通信が中断しました。再接続してください。".into())
            }
        }
    }

    pub async fn targets(&mut self) -> Result<Vec<Target>, String> {
        serde_json::from_value(
            self.call(None, "Target.getTargets", json!({})).await?["targetInfos"].clone(),
        )
        .map_err(|_| "ブラウザのタブ一覧を取得できません。".into())
    }
    pub async fn create(&mut self) -> Result<String, String> {
        self.call(None, "Target.createTarget", json!({"url":"about:blank"}))
            .await?["targetId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "ブラウザのタブを作成できません。".into())
    }
    pub async fn attach(&mut self, target: &str) -> Result<String, String> {
        if let Some(session) = self.sessions.get(target) {
            return Ok(session.clone());
        }
        let response = self
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
            )
            .await?;
        let session = response["sessionId"]
            .as_str()
            .ok_or("ブラウザのタブに接続できません。")?
            .to_owned();
        self.call(Some(&session), "Page.enable", json!({})).await?;
        self.call(
            Some(&session),
            "Emulation.setDeviceMetricsOverride",
            json!({"width":WIDTH,"height":HEIGHT,"deviceScaleFactor":1,"mobile":false}),
        )
        .await?;
        self.sessions.insert(target.into(), session.clone());
        Ok(session)
    }
    pub fn dialog(&self, session: &str) -> Option<BrowserDialog> {
        self.dialogs.get(session).cloned()
    }
    pub async fn screenshot(&mut self, session: &str) -> Result<Vec<u8>, String> {
        self.call(Some(session), "Page.bringToFront", json!({}))
            .await?;
        let mut retries = 0;
        let frame = loop {
            let result = self
                .call(
                    Some(session),
                    "Page.captureScreenshot",
                    json!({"format":"jpeg","quality":65,"captureBeyondViewport":false}),
                )
                .await;
            if result.is_ok() || self.broken || retries == 2 {
                break result?;
            }
            // Navigation can briefly leave no active page to capture. Retry
            // only the frame capture, never the user's navigation or input.
            retries += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let data = frame["data"]
            .as_str()
            .filter(|s| s.len() <= 4 * 1024 * 1024)
            .ok_or("ブラウザ画像が大きすぎます。")?;
        STANDARD
            .decode(data)
            .map_err(|_| "ブラウザ画像を読み取れません。".into())
    }
    pub async fn action(&mut self, session: &str, action: &BrowserAction) -> Result<(), String> {
        self.call(Some(session), "Page.bringToFront", json!({}))
            .await?;
        let (method, params) = match action {
            BrowserAction::Navigate { url } => (
                "Page.navigate",
                json!({"url":agent_core::presentation::browser::browser_url(url)?}),
            ),
            BrowserAction::Reload => ("Page.reload", json!({})),
            BrowserAction::Click { x, y } => {
                self.call(
                    Some(session),
                    "Input.dispatchMouseEvent",
                    json!({"type":"mousePressed","x":x,"y":y,"button":"left","clickCount":1}),
                )
                .await?;
                (
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseReleased","x":x,"y":y,"button":"left","clickCount":1}),
                )
            }
            BrowserAction::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => (
                "Input.dispatchMouseEvent",
                json!({"type":"mouseWheel","x":x,"y":y,"deltaX":delta_x,"deltaY":delta_y}),
            ),
            BrowserAction::Type { text } => ("Input.insertText", json!({"text":text})),
            BrowserAction::Key { key } => {
                let (name, code, modifiers) = match key {
                    BrowserKey::Enter => ("Enter", 13, 0),
                    BrowserKey::Tab => ("Tab", 9, 0),
                    BrowserKey::Backspace => ("Backspace", 8, 0),
                    BrowserKey::Escape => ("Escape", 27, 0),
                    BrowserKey::ArrowUp => ("ArrowUp", 38, 0),
                    BrowserKey::ArrowDown => ("ArrowDown", 40, 0),
                    BrowserKey::ArrowLeft => ("ArrowLeft", 37, 0),
                    BrowserKey::ArrowRight => ("ArrowRight", 39, 0),
                    BrowserKey::SelectAll => {
                        ("a", 65, if cfg!(target_os = "macos") { 4 } else { 2 })
                    }
                };
                let mut params = json!({"type":"keyDown","key":name,"windowsVirtualKeyCode":code,"modifiers":modifiers});
                if matches!(key, BrowserKey::Enter) {
                    params["text"] = "\r".into();
                }
                self.call(Some(session), "Input.dispatchKeyEvent", params.clone())
                    .await?;
                params["type"] = "keyUp".into();
                params.as_object_mut().unwrap().remove("text");
                ("Input.dispatchKeyEvent", params)
            }
            BrowserAction::Back | BrowserAction::Forward => {
                let history = self
                    .call(Some(session), "Page.getNavigationHistory", json!({}))
                    .await?;
                let offset = if matches!(action, BrowserAction::Back) {
                    -1
                } else {
                    1
                };
                let index = history["currentIndex"].as_i64().unwrap_or(0) + offset;
                let Some(entry) = usize::try_from(index)
                    .ok()
                    .and_then(|i| history["entries"].get(i))
                else {
                    return Ok(());
                };
                (
                    "Page.navigateToHistoryEntry",
                    json!({"entryId":entry["id"]}),
                )
            }
            BrowserAction::Dialog { accept, text } => (
                "Page.handleJavaScriptDialog",
                json!({"accept":accept,"promptText":text}),
            ),
            _ => return Ok(()),
        };
        let result = self.call(Some(session), method, params).await?;
        if method == "Page.navigate" && result.get("errorText").is_some() {
            return Err(
                "ページを開けません。URLとMacのネットワーク接続を確認してください。".into(),
            );
        }
        Ok(())
    }
}
