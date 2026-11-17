use super::ActionOutput;
use agent_protocol::browser::{BrowserAction, BrowserDialog, BrowserKey, HEIGHT, WIDTH};
use async_tungstenite::{WebSocketStream, tokio::ConnectStream, tungstenite::Message};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, hash_map::Entry},
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::process::Child;
use zeroize::Zeroizing;

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

struct CachedFrame {
    image: Vec<u8>,
    width: u32,
    height: u32,
}

struct Viewer {
    seen: Instant,
    streaming: bool,
}

#[derive(Default)]
struct Events {
    dialogs: HashMap<String, BrowserDialog>,
    frames: HashMap<String, CachedFrame>,
    viewers: HashMap<String, Viewer>,
    loading: HashMap<String, bool>,
    navigation: HashMap<String, u64>,
}
struct Command {
    request: Value,
    reply: tokio::sync::oneshot::Sender<Value>,
}

pub(super) struct Chrome {
    child: Child,
    commands: tokio::sync::mpsc::Sender<Command>,
    events: Arc<Mutex<Events>>,
    transport: tokio::task::JoinHandle<()>,
    next_id: u64,
    sessions: HashMap<String, String>,
    sizes: HashMap<String, (u32, u32)>,
    refs: HashMap<String, (u64, HashMap<String, i64>)>,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        self.transport.abort();
    }
}

impl Chrome {
    fn connected(child: Child, mut socket: WebSocketStream<ConnectStream>) -> Self {
        let (commands, mut incoming) = tokio::sync::mpsc::channel::<Command>(16);
        let events = Arc::new(Mutex::new(Events::default()));
        let updates = events.clone();
        let transport = tokio::spawn(async move {
            let mut pending: HashMap<u64, tokio::sync::oneshot::Sender<Value>> = HashMap::new();
            let mut cleanup = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = cleanup.tick() => {
                        pending.retain(|_, reply| !reply.is_closed());
                        let expired: Vec<_> = {
                            let mut events = updates.lock().unwrap();
                            let expired: Vec<_> = events.viewers.iter().filter(|(_, viewer)| viewer.seen.elapsed() >= Duration::from_secs(3)).map(|(session,viewer)| (session.clone(), viewer.streaming)).collect();
                            for (session, _) in &expired { events.viewers.remove(session); events.frames.remove(session); }
                            expired
                        };
                        for (session, streaming) in expired {
                            if !streaming { continue; }
                            let request = json!({"id":0,"sessionId":session,"method":"Page.stopScreencast","params":{}});
                            if socket.send(Message::Text(request.to_string().into())).await.is_err() { return; }
                        }
                    }
                    command = incoming.recv() => {
                        let Some(command) = command else { break; };
                        if command.reply.is_closed() { continue; }
                        let id = command.request["id"].as_u64().unwrap();
                        if socket.send(Message::Text(command.request.to_string().into())).await.is_err() { break; }
                        pending.insert(id, command.reply);
                    }
                    message = socket.next() => {
                        let Some(Ok(message)) = message else { break; };
                        let Message::Text(text) = message else { if matches!(message, Message::Close(_)) { break; } continue; };
                        let Ok(value) = serde_json::from_str::<Value>(&text) else { break; };
                        if let Some(id) = value["id"].as_u64() {
                            if let Some(reply) = pending.remove(&id) { let _ = reply.send(value); }
                            continue;
                        }
                        let Some(session) = value["sessionId"].as_str() else { continue; };
                        let params = &value["params"];
                        let acknowledgement = {
                            let mut events = updates.lock().unwrap();
                            match value["method"].as_str() {
                                Some("Page.javascriptDialogOpening") => { events.dialogs.insert(session.into(), BrowserDialog { message: params["message"].as_str().unwrap_or_default().chars().take(4096).collect(), prompt: params["type"] == "prompt" }); }
                                Some("Page.javascriptDialogClosed") => { events.dialogs.remove(session); }
                                Some("Page.frameStartedLoading") => { events.loading.insert(session.into(), true); }
                                Some("Page.frameStoppedLoading") => { events.loading.insert(session.into(), false); }
                                Some("Page.frameNavigated") if params["frame"]["parentId"].is_null() => { *events.navigation.entry(session.into()).or_default() += 1; }
                                Some("Page.screencastFrame") => {
                                    if let Some(data) = params["data"].as_str().filter(|data| data.len() <= 4 * 1024 * 1024)
                                        && events.viewers.get(session).is_some_and(|viewer| viewer.streaming)
                                        && let Ok(image) = STANDARD.decode(data) { events.frames.insert(session.into(), CachedFrame { image, width:params["metadata"]["deviceWidth"].as_f64().unwrap_or(0.) as u32, height:params["metadata"]["deviceHeight"].as_f64().unwrap_or(0.) as u32 }); }
                                }
                                _ => (),
                            }
                            (value["method"] == "Page.screencastFrame").then(|| json!({"id":0,"sessionId":session,"method":"Page.screencastFrameAck","params":{"sessionId":params["sessionId"]}}))
                        };
                        if let Some(ack) = acknowledgement && socket.send(Message::Text(ack.to_string().into())).await.is_err() { break; }
                    }
                }
            }
        });
        Self {
            child,
            commands,
            events,
            transport,
            next_id: 0,
            sessions: HashMap::new(),
            sizes: HashMap::new(),
            refs: HashMap::new(),
        }
    }

    pub(super) async fn launch(profile: &Path, executable: &Path) -> Result<Self, String> {
        agent_local_host::create_state_directory(profile).map_err(|e| e.to_string())?;
        let port_file = profile.join("DevToolsActivePort");
        match tokio::fs::remove_file(&port_file).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let started = Instant::now();
        tracing::info!(target: "agent", operation = "browser.launch", message = "Chrome starting");
        let mut child = agent_process::command(executable)
            .map_err(|e| e.to_string())?
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--remote-debugging-address=127.0.0.1",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--no-startup-window",
                "--window-size=1280,800",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Bexブラウザを起動できません: {e}"))?;
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    return Err("Bexブラウザが起動中に終了しました。".into());
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
                        tracing::info!(target: "agent", operation = "browser.launch",
                            message = %format_args!("DevTools endpoint published after {} ms", started.elapsed().as_millis()));
                        let (socket, _) = async_tungstenite::tokio::connect_async(url)
                            .await
                            .map_err(|_| "Bexブラウザに接続できません。".to_owned())?;
                        return Ok(socket);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| Err("Bexブラウザの起動がタイムアウトしました。".into()));
        match result {
            Ok(socket) => {
                tracing::info!(target: "agent", operation = "browser.launch",
                    message = %format_args!("Chrome connected after {} ms", started.elapsed().as_millis()));
                Ok(Self::connected(child, socket))
            }
            Err(error) => {
                tracing::warn!(target: "agent", operation = "browser.launch",
                    message = %format_args!("Chrome launch failed after {} ms: {error}", started.elapsed().as_millis()));
                child.stdin.take();
                let _ = child.wait().await;
                Err(error)
            }
        }
    }

    pub(super) async fn shutdown(mut self) {
        let _ = self.call(None, "Browser.close", json!({})).await;
        // Keep the supervisor lifetime pipe open while Chrome flushes its profile.
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await,
            Ok(Ok(_))
        ) {
            self.child.stdin.take();
            let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        }
    }

    pub(super) async fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        if self.disconnected() {
            return Err("Bexブラウザとの接続が切れました。".into());
        }
        self.next_id += 1;
        let id = self.next_id;
        let mut request = json!({"id":id,"method":method,"params":params});
        if let Some(session) = session {
            request["sessionId"] = session.into();
        }
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let (reply, received) = tokio::sync::oneshot::channel();
            self.commands
                .send(Command { request, reply })
                .await
                .map_err(|_| ())?;
            received.await.map_err(|_| ())
        })
        .await;
        match result {
            Ok(Ok(value)) if value.get("error").is_none() => {
                if started.elapsed() >= Duration::from_secs(1) {
                    tracing::info!(target: "agent", operation = "browser.cdp",
                        message = %format_args!("{method} completed after {} ms", started.elapsed().as_millis()));
                }
                Ok(value["result"].clone())
            }
            Ok(Ok(value)) => {
                tracing::debug!(target: "agent", operation = "browser.cdp",
                    error_code = value["error"]["code"].as_i64(),
                    message = %format_args!("{method} rejected"));
                Err(format!(
                    "ブラウザ操作を完了できません（{method}）。画面を確認して再試行してください。"
                ))
            }
            Err(_) => {
                tracing::warn!(target: "agent", operation = "browser.cdp",
                    message = %format_args!("{method} timed out after {} ms", started.elapsed().as_millis()));
                // The command may still complete. Keep the transport and tabs,
                // and never replay an operation whose outcome is uncertain.
                Err("ブラウザ操作の応答がタイムアウトしました。操作が完了している可能性があるため、画面を確認してください。".into())
            }
            Ok(Err(())) => {
                tracing::warn!(target: "agent", operation = "browser.cdp",
                    message = %format_args!("{method} interrupted after {} ms", started.elapsed().as_millis()));
                Err("Bexブラウザとの通信が中断しました。再接続してください。".into())
            }
        }
    }

    pub(super) fn disconnected(&self) -> bool {
        self.commands.is_closed()
    }

    #[cfg(test)]
    pub(super) async fn install_cookies(
        &mut self,
        cookies: &[super::credentials::Cookie],
    ) -> Result<(), String> {
        if cookies.is_empty() {
            return Ok(());
        }
        let parameters: Vec<_> = cookies.iter().map(cookie_params).collect();
        self.call(None, "Storage.setCookies", json!({"cookies":parameters}))
            .await?;
        let installed = self.call(None, "Storage.getCookies", json!({})).await?;
        let verified = installed["cookies"]
            .as_array()
            .is_some_and(|installed| cookies_installed(&parameters, installed));
        if !verified {
            return Err("一部のCookieを元の属性で保存できませんでした。保存済みのCookieもあります。再試行してください。".into());
        }
        Ok(())
    }

    pub(super) async fn targets(&mut self) -> Result<Vec<Target>, String> {
        let targets: Vec<Target> = serde_json::from_value(
            self.call(None, "Target.getTargets", json!({})).await?["targetInfos"].clone(),
        )
        .map_err(|_| "ブラウザのタブ一覧を取得できません。".to_owned())?;
        let removed: Vec<_> = self
            .sessions
            .keys()
            .filter(|id| !targets.iter().any(|target| &target.target_id == *id))
            .cloned()
            .collect();
        for target in removed {
            self.forget_target(&target);
        }
        Ok(targets)
    }
    pub(super) async fn create(&mut self) -> Result<String, String> {
        self.call(None, "Target.createTarget", json!({"url":"about:blank"}))
            .await?["targetId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "ブラウザのタブを作成できません。".into())
    }
    pub(super) async fn close_tab(&mut self, target: &str) -> Result<(), String> {
        self.call(None, "Target.closeTarget", json!({"targetId":target}))
            .await?;
        self.forget_target(target);
        Ok(())
    }
    fn forget_target(&mut self, target: &str) {
        if let Some(session) = self.sessions.remove(target) {
            let mut events = self.events.lock().unwrap();
            events.dialogs.remove(&session);
            events.frames.remove(&session);
            events.viewers.remove(&session);
            events.loading.remove(&session);
            events.navigation.remove(&session);
            self.sizes.remove(&session);
            self.refs.remove(&session);
        }
    }

    pub(super) fn navigation(&self, target: &str) -> u64 {
        self.sessions
            .get(target)
            .and_then(|session| self.events.lock().unwrap().navigation.get(session).copied())
            .unwrap_or(0)
    }
    pub(super) async fn resize(
        &mut self,
        session: &str,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if self.sizes.get(session) == Some(&(width, height)) {
            return Ok(());
        }
        self.call(
            Some(session),
            "Emulation.setDeviceMetricsOverride",
            json!({"width":width,"height":height,"deviceScaleFactor":1,"mobile":false}),
        )
        .await?;
        self.events.lock().unwrap().frames.remove(session);
        self.sizes.insert(session.into(), (width, height));
        Ok(())
    }
    pub(super) async fn history(&mut self, session: &str) -> Result<(bool, bool), String> {
        let result = self
            .read_page(session, "Page.getNavigationHistory", json!({}))
            .await?;
        let index = result["currentIndex"].as_u64().unwrap_or(0) as usize;
        Ok((
            index > 0,
            result["entries"]
                .as_array()
                .is_some_and(|entries| index + 1 < entries.len()),
        ))
    }
    pub(super) async fn attach(&mut self, target: &str) -> Result<String, String> {
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
        self.resize(&session, WIDTH, HEIGHT).await?;
        self.sessions.insert(target.into(), session.clone());
        Ok(session)
    }
    pub(super) fn dialog(&self, session: &str) -> Option<BrowserDialog> {
        self.events.lock().unwrap().dialogs.get(session).cloned()
    }
    pub(super) fn loading(&self, session: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .loading
            .get(session)
            .copied()
            .unwrap_or(false)
    }
    pub(super) async fn latest_image(&mut self, session: &str) -> Result<Vec<u8>, String> {
        let (start, streaming) = {
            let mut events = self.events.lock().unwrap();
            match events.viewers.entry(session.into()) {
                Entry::Occupied(mut entry) => {
                    entry.get_mut().seen = Instant::now();
                    (false, entry.get().streaming)
                }
                Entry::Vacant(entry) => {
                    entry.insert(Viewer {
                        seen: Instant::now(),
                        streaming: true,
                    });
                    (true, true)
                }
            }
        };
        let streaming = if start
            && self
                .call(
                    Some(session),
                    "Page.startScreencast",
                    json!({"format":"jpeg","quality":70,"everyNthFrame":1}),
                )
                .await
                .is_err()
        {
            // Starting may have completed before a timeout; stop it before switching capture modes.
            let _ = self
                .call(Some(session), "Page.stopScreencast", json!({}))
                .await;
            self.events.lock().unwrap().viewers.insert(
                session.into(),
                Viewer {
                    seen: Instant::now(),
                    streaming: false,
                },
            );
            false
        } else {
            streaming
        };
        let size = self.sizes.get(session).copied().unwrap_or((WIDTH, HEIGHT));
        if streaming && let Some(image) = self.cached_image(session) {
            return Ok(image);
        }
        let image = self.screenshot(session).await?;
        self.events.lock().unwrap().frames.insert(
            session.into(),
            CachedFrame {
                image: image.clone(),
                width: size.0,
                height: size.1,
            },
        );
        Ok(image)
    }
    pub(super) fn cached_image(&self, session: &str) -> Option<Vec<u8>> {
        let size = self.sizes.get(session).copied().unwrap_or((WIDTH, HEIGHT));
        self.events
            .lock()
            .unwrap()
            .frames
            .get(session)
            .filter(|frame| (frame.width, frame.height) == size)
            .map(|frame| frame.image.clone())
    }
    pub(super) async fn screenshot(&mut self, session: &str) -> Result<Vec<u8>, String> {
        self.call(Some(session), "Page.bringToFront", json!({}))
            .await?;
        let frame = self
            .read_page(
                session,
                "Page.captureScreenshot",
                json!({"format":"jpeg","quality":65,"captureBeyondViewport":false}),
            )
            .await?;
        let data = frame["data"]
            .as_str()
            .filter(|s| s.len() <= 4 * 1024 * 1024)
            .ok_or("ブラウザ画像が大きすぎます。")?;
        STANDARD
            .decode(data)
            .map_err(|_| "ブラウザ画像を読み取れません。".into())
    }
    async fn read_page(
        &mut self,
        session: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let result = self.call(Some(session), method, params.clone()).await;
            if result.is_ok() || self.disconnected() || tokio::time::Instant::now() >= deadline {
                return result;
            }
            // A navigation can briefly leave no active page for history or
            // capture. Retry only reads, never navigation or user input.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub(super) async fn evaluate(
        &mut self,
        session: &str,
        expression: &str,
    ) -> Result<Value, String> {
        let result = self.call(Some(session), "Runtime.evaluate", json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"timeout":3000})).await?;
        if result.get("exceptionDetails").is_some() {
            return Err(
                "ページ内のスクリプトを完了できません。要素とページの状態を確認してください。"
                    .into(),
            );
        }
        Ok(result["result"]["value"].clone())
    }
    async fn element(
        &mut self,
        session: &str,
        selector: &str,
        function: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let object = if selector.starts_with("ref=") {
            let id = self
                .refs
                .get(session)
                .filter(|(generation, _)| {
                    *generation
                        == self
                            .events
                            .lock()
                            .unwrap()
                            .navigation
                            .get(session)
                            .copied()
                            .unwrap_or(0)
                })
                .and_then(|(_, refs)| refs.get(selector))
                .ok_or("要素参照が失効しました。新しい snapshot を取得してください。")?;
            self.call(
                Some(session),
                "DOM.resolveNode",
                json!({"backendNodeId":id}),
            )
            .await?["object"]["objectId"]
                .as_str()
                .map(str::to_owned)
                .ok_or("要素が見つかりません。")?
        } else {
            let expression = format!(
                "(() => {{ const nodes = document.querySelectorAll({}); if (nodes.length !== 1) throw new Error('Expected exactly one element'); return nodes[0]; }})()",
                serde_json::to_string(selector).unwrap()
            );
            let result = self
                .call(
                    Some(session),
                    "Runtime.evaluate",
                    json!({"expression":expression}),
                )
                .await?;
            if result.get("exceptionDetails").is_some() {
                return Err("セレクターは1つの要素に一致する必要があります。".into());
            }
            result["result"]["objectId"]
                .as_str()
                .map(str::to_owned)
                .ok_or("要素が見つかりません。")?
        };
        let result = self.call(Some(session), "Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":function,"arguments":arguments,"returnByValue":true,"userGesture":true})).await;
        let _ = self
            .call(
                Some(session),
                "Runtime.releaseObject",
                json!({"objectId":object}),
            )
            .await;
        let result = result?;
        if result.get("exceptionDetails").is_some() {
            return Err("この要素を操作できません。ページを確認してください。".into());
        }
        Ok(result["result"]["value"].clone())
    }

    async fn mouse_click(&mut self, session: &str, x: f64, y: f64) -> Result<(), String> {
        for kind in ["mousePressed", "mouseReleased"] {
            self.call(
                Some(session),
                "Input.dispatchMouseEvent",
                json!({"type":kind,"x":x,"y":y,"button":"left","clickCount":1}),
            )
            .await?;
        }
        Ok(())
    }
    async fn snapshot(&mut self, session: &str) -> Result<String, String> {
        let navigation = self
            .events
            .lock()
            .unwrap()
            .navigation
            .get(session)
            .copied()
            .unwrap_or(0);
        let protected = self.evaluate(session, r#"(() => { const values=[]; const visit=root=>{ for(const e of root.querySelectorAll('*')) { if((e.tagName==='INPUT'&&e.type==='password') || (getComputedStyle(e).webkitTextSecurity && getComputedStyle(e).webkitTextSecurity!=='none')) { const value=e.value??e.textContent??''; if(value.length>16384)throw new Error('Protected field too long'); if(value)values.push(value); } if(e.shadowRoot)visit(e.shadowRoot); if(e.tagName==='IFRAME'){try{if(e.contentDocument)visit(e.contentDocument);}catch{}} } }; visit(document); return values; })()"#).await?;
        let secrets: Vec<_> = protected
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|value| Zeroizing::new(value.to_owned()))
            .collect();
        let result = self
            .call(Some(session), "Accessibility.getFullAXTree", json!({}))
            .await?;
        let generation = uuid::Uuid::new_v4();
        let mut refs = HashMap::new();
        let mut nodes = Vec::new();
        let mut bytes = 0;
        if let Some(tree) = result["nodes"].as_array() {
            for node in tree {
                if node["ignored"] == true {
                    continue;
                }
                let mut item = json!({"role":node["role"]["value"],"name":node["name"]["value"],"value":node["value"]["value"],"properties":node["properties"]});
                item = redact(item, &secrets);
                if let Some(id) = node["backendDOMNodeId"].as_i64() {
                    let reference = format!("ref={generation}-{id}");
                    refs.insert(reference.clone(), id);
                    item["ref"] = reference.into();
                }
                bytes += item.to_string().len();
                if bytes > 20 * 1024 {
                    nodes.push(json!({"truncated":true}));
                    break;
                }
                nodes.push(item);
            }
        }
        if navigation
            != self
                .events
                .lock()
                .unwrap()
                .navigation
                .get(session)
                .copied()
                .unwrap_or(0)
        {
            return Err("ページが移動しました。新しい snapshot を取得してください。".into());
        }
        self.refs.insert(session.into(), (navigation, refs));
        Ok(serde_json::to_string(&nodes).unwrap())
    }
    pub(super) async fn action(
        &mut self,
        session: &str,
        action: &BrowserAction,
    ) -> Result<ActionOutput, String> {
        if matches!(action, BrowserAction::Snapshot) && self.dialog(session).is_some() {
            return Ok(ActionOutput::default());
        }
        self.call(Some(session), "Page.bringToFront", json!({}))
            .await?;
        let (method, params) = match action {
            BrowserAction::Snapshot => {
                return Ok(ActionOutput {
                    page_text: self.snapshot(session).await?,
                    ..Default::default()
                });
            }
            BrowserAction::Evaluate { expression } => {
                let value = self.evaluate(session, expression).await?.to_string();
                if value.len() > 128 * 1024 {
                    return Err("結果が大きすぎます。取得する範囲を絞ってください。".into());
                }
                return Ok(ActionOutput {
                    value,
                    ..Default::default()
                });
            }
            BrowserAction::ClickElement { selector } => {
                let point = self.element(session, selector, "function(){if(this.disabled)throw new Error('Disabled');this.scrollIntoView({block:'center',inline:'center'});let view=this.ownerDocument.defaultView;const r=this.getBoundingClientRect(),l=Math.max(0,r.left),t=Math.max(0,r.top),right=Math.min(view.innerWidth,r.right),bottom=Math.min(view.innerHeight,r.bottom);if(l>=right||t>=bottom||view.getComputedStyle(this).visibility==='hidden')throw new Error('Not visible');let x=(l+right)/2,y=(t+bottom)/2;const root=this.getRootNode(),hit=root.elementFromPoint(x,y);if(hit!==this&&!this.contains(hit))throw new Error('Covered');while(view!==view.top){const f=view.frameElement;if(!f)throw new Error('Frame unavailable');const b=f.getBoundingClientRect();x=b.left+(f.clientLeft+x)*b.width/f.offsetWidth;y=b.top+(f.clientTop+y)*b.height/f.offsetHeight;view=view.parent;}return{x,y};}", json!([])).await?;
                let (Some(x), Some(y)) = (point["x"].as_f64(), point["y"].as_f64()) else {
                    return Err("要素の位置を取得できません。".into());
                };
                let (width, height) = self.sizes.get(session).copied().unwrap_or((WIDTH, HEIGHT));
                if !(0.0..f64::from(width)).contains(&x) || !(0.0..f64::from(height)).contains(&y) {
                    return Err("要素が表示範囲外です。".into());
                }
                self.mouse_click(session, x, y).await?;
                return Ok(ActionOutput::default());
            }
            BrowserAction::Fill { selector, text } => {
                self.element(session, selector, "function(text){if(this.disabled || this.readOnly) throw new Error('Not editable'); this.focus(); if(this.isContentEditable){ this.textContent=text; } else { const prototype=this.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype; const setter=Object.getOwnPropertyDescriptor(prototype,'value').set; setter.call(this,text); } this.dispatchEvent(new Event('input',{bubbles:true})); this.dispatchEvent(new Event('change',{bubbles:true}));}", json!([{"value":text}])).await?;
                return Ok(ActionOutput::default());
            }
            BrowserAction::SelectOption { selector, value } => {
                self.element(session, selector, "function(value){if(this.tagName!=='SELECT'||this.disabled)throw new Error('Not a select'); const options=[...this.options].filter(o=>o.value===value||o.label===value); if(options.length!==1)throw new Error('Ambiguous option'); this.value=options[0].value;this.dispatchEvent(new Event('input',{bubbles:true}));this.dispatchEvent(new Event('change',{bubbles:true}));}", json!([{"value":value}])).await?;
                return Ok(ActionOutput::default());
            }
            BrowserAction::Hover { x, y } => (
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":x,"y":y}),
            ),
            BrowserAction::Drag { x, y, to_x, to_y } => {
                self.call(Some(session), "Input.dispatchMouseEvent", json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1})).await?;
                self.call(
                    Some(session),
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseMoved","x":to_x,"y":to_y,"button":"left","buttons":1}),
                )
                .await?;
                (
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseReleased","x":to_x,"y":to_y,"button":"left","buttons":0,"clickCount":1}),
                )
            }
            BrowserAction::Navigate { url } => (
                "Page.navigate",
                json!({"url":agent_protocol::browser::browser_url(url)?}),
            ),
            BrowserAction::Reload => ("Page.reload", json!({})),
            BrowserAction::Click { x, y } => {
                self.mouse_click(session, *x, *y).await?;
                return Ok(ActionOutput::default());
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
                let (name, code, modifiers) = key_event(*key);
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
                    .read_page(session, "Page.getNavigationHistory", json!({}))
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
                    return Ok(ActionOutput::default());
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
            _ => return Ok(ActionOutput::default()),
        };
        let result = self.call(Some(session), method, params).await?;
        if method == "Page.navigate" && result.get("errorText").is_some() {
            return Err(
                "ページを開けません。URLとMacのネットワーク接続を確認してください。".into(),
            );
        }
        if matches!(
            action,
            BrowserAction::Navigate { .. }
                | BrowserAction::Back
                | BrowserAction::Forward
                | BrowserAction::Reload
        ) {
            self.refs.remove(session);
        }
        Ok(ActionOutput::default())
    }
}

fn key_event(key: BrowserKey) -> (&'static str, u32, u32) {
    match key {
        BrowserKey::Enter => ("Enter", 13, 0),
        BrowserKey::Tab => ("Tab", 9, 0),
        BrowserKey::Backspace => ("Backspace", 8, 0),
        BrowserKey::Escape => ("Escape", 27, 0),
        BrowserKey::ArrowUp => ("ArrowUp", 38, 0),
        BrowserKey::ArrowDown => ("ArrowDown", 40, 0),
        BrowserKey::ArrowLeft => ("ArrowLeft", 37, 0),
        BrowserKey::ArrowRight => ("ArrowRight", 39, 0),
        BrowserKey::SelectAll => ("a", 65, 2),
        BrowserKey::Delete => ("Delete", 46, 0),
        BrowserKey::Home => ("Home", 36, 0),
        BrowserKey::End => ("End", 35, 0),
        BrowserKey::PageUp => ("PageUp", 33, 0),
        BrowserKey::PageDown => ("PageDown", 34, 0),
    }
}

fn redact(value: Value, secrets: &[Zeroizing<String>]) -> Value {
    match value {
        Value::String(mut value) => {
            for secret in secrets {
                value = value.replace(&**secret, "••••");
            }
            Value::String(value)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact(value, secrets))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, redact(value, secrets)))
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
fn cookie_params(cookie: &super::credentials::Cookie) -> serde_json::Value {
    let mut value = serde_json::json!({"domain":cookie.domain,"name":cookie.name,"value":cookie.value.as_str(),"path":cookie.path,"secure":cookie.secure,"httpOnly":cookie.http_only});
    if let Some(expires) = cookie.expires {
        value["expires"] = expires.into();
    }
    if let Some(policy) = match cookie.same_site {
        0 => Some("None"),
        1 => Some("Lax"),
        2 => Some("Strict"),
        _ => None,
    } {
        value["sameSite"] = policy.into();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Failure {
        Reject(usize),
        Disconnect,
        Defer,
    }

    async fn fixture(
        failure_method: &'static str,
        failure: Failure,
    ) -> (
        Chrome,
        tokio::task::JoinHandle<Vec<String>>,
        tokio::sync::mpsc::UnboundedSender<Value>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (events, mut injected) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = async_tungstenite::tokio::accept_async(stream)
                .await
                .unwrap();
            let mut methods = Vec::new();
            let mut attempts = 0;
            loop {
                let message = tokio::select! {
                    Some(event) = injected.recv() => { socket.send(Message::Text(event.to_string().into())).await.unwrap(); continue; }
                    message = socket.next() => message,
                };
                let Some(message) = message else {
                    break;
                };
                let Message::Text(text) = message.unwrap() else {
                    break;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                let method = request["method"].as_str().unwrap();
                methods.push(method.to_owned());
                let failure = if method == failure_method {
                    attempts += 1;
                    match failure {
                        Failure::Disconnect => break,
                        Failure::Defer => continue,
                        Failure::Reject(count) => attempts <= count,
                    }
                } else {
                    false
                };
                let response = if failure {
                    json!({"id":request["id"],"error":{"code":-32000,"message":"Not attached to an active page"}})
                } else {
                    let result = match method {
                        "Page.captureScreenshot" => json!({"data":"/9j/2Q=="}),
                        "Page.getNavigationHistory" => {
                            json!({"currentIndex":2,"entries":[{"id":11},{"id":12},{"id":13}]})
                        }
                        "Target.getTargets" => json!({"targetInfos":[]}),
                        _ => json!({}),
                    };
                    json!({"id":request["id"],"result":result})
                };
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
                if method == "Browser.close" {
                    break;
                }
            }
            methods
        });
        let (socket, _) = async_tungstenite::tokio::connect_async(format!("ws://{address}"))
            .await
            .unwrap();
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        (Chrome::connected(child, socket), server, events)
    }

    #[test]
    fn snapshots_redact_protected_text_in_nested_accessibility_values() {
        let value =
            json!({"name":"label synthetic-value","properties":[{"value":"synthetic-value"}]});
        let result = redact(value, &[Zeroizing::new("synthetic-value".into())]);
        assert!(!result.to_string().contains("synthetic-value"));
        assert_eq!(result["name"], "label ••••");
    }

    #[tokio::test]
    async fn closed_targets_release_frames_dialogs_and_element_references() {
        let (mut chrome, server, _) = fixture("Page.captureScreenshot", Failure::Reject(0)).await;
        chrome.sessions.insert("closed".into(), "page".into());
        chrome
            .refs
            .insert("page".into(), (0, HashMap::from([("ref=old".into(), 1)])));
        chrome.latest_image("page").await.unwrap();
        assert!(chrome.cached_image("page").is_some());
        assert!(chrome.targets().await.unwrap().is_empty());
        assert!(chrome.cached_image("page").is_none());
        assert!(
            chrome
                .element("page", "ref=old", "function(){}", json!([]))
                .await
                .is_err()
        );
        chrome.shutdown().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn streamed_frames_replace_idle_cache_without_polling_screenshots() {
        let (mut chrome, server, events) =
            fixture("Page.captureScreenshot", Failure::Reject(0)).await;
        assert_eq!(
            chrome.latest_image("page").await.unwrap(),
            [0xff, 0xd8, 0xff, 0xd9]
        );
        events.send(json!({"sessionId":"page","method":"Page.screencastFrame","params":{"sessionId":7,"data":"/9j/2QAB","metadata":{"deviceWidth":WIDTH,"deviceHeight":HEIGHT}}})).unwrap();
        let latest = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let image = chrome.latest_image("page").await.unwrap();
                if image.len() == 6 {
                    break image;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(latest, [0xff, 0xd8, 0xff, 0xd9, 0, 1]);
        events.send(json!({"sessionId":"page","method":"Page.javascriptDialogOpening","params":{"message":"fixture dialog","type":"alert"}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while chrome.dialog("page").is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            chrome
                .action("page", &BrowserAction::Snapshot)
                .await
                .unwrap()
                .page_text
                .is_empty()
        );
        chrome.shutdown().await;
        let methods = server.await.unwrap();
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "Page.captureScreenshot")
                .count(),
            1
        );
        assert!(
            methods
                .iter()
                .any(|method| method == "Page.screencastFrameAck")
        );
        assert!(
            !methods.iter().any(|method| method == "Runtime.evaluate"),
            "inspection must not block on page scripts behind a modal dialog"
        );
    }

    #[tokio::test]
    async fn snapshot_viewers_refresh_and_expire_without_replaying_actions() {
        let (mut chrome, server, _) =
            fixture("Page.startScreencast", Failure::Reject(usize::MAX)).await;
        chrome.sessions.insert("target".into(), "page".into());
        chrome
            .action(
                "page",
                &BrowserAction::Navigate {
                    url: "https://example.test/".into(),
                },
            )
            .await
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                chrome.latest_image("page").await.unwrap(),
                [0xff, 0xd8, 0xff, 0xd9]
            );
            assert_eq!(
                chrome.cached_image("page").unwrap(),
                [0xff, 0xd8, 0xff, 0xd9]
            );
        }
        chrome
            .events
            .lock()
            .unwrap()
            .viewers
            .get_mut("page")
            .unwrap()
            .seen = Instant::now() - Duration::from_secs(4);
        tokio::time::timeout(Duration::from_secs(3), async {
            while chrome.cached_image("page").is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!chrome.events.lock().unwrap().viewers.contains_key("page"));
        assert_eq!(
            chrome.latest_image("page").await.unwrap(),
            [0xff, 0xd8, 0xff, 0xd9]
        );
        assert!(chrome.targets().await.unwrap().is_empty());
        assert!(!chrome.events.lock().unwrap().viewers.contains_key("page"));
        assert!(chrome.cached_image("page").is_none());
        chrome.shutdown().await;
        let methods = server.await.unwrap();
        for (method, count) in [
            ("Page.startScreencast", 2),
            ("Page.captureScreenshot", 3),
            ("Page.stopScreencast", 2),
            ("Page.navigate", 1),
        ] {
            assert_eq!(
                methods.iter().filter(|sent| *sent == method).count(),
                count,
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn history_waits_for_an_active_page_without_replaying_navigation() {
        for action in [
            None,
            Some(BrowserAction::Back),
            Some(BrowserAction::Forward),
        ] {
            let (mut chrome, server, _) =
                fixture("Page.getNavigationHistory", Failure::Reject(3)).await;
            chrome
                .action(
                    "page",
                    &BrowserAction::Navigate {
                        url: "https://example.invalid/".into(),
                    },
                )
                .await
                .unwrap();
            if let Some(action) = &action {
                chrome.action("page", action).await.unwrap();
            } else {
                assert_eq!(chrome.history("page").await.unwrap(), (true, false));
            }
            chrome.shutdown().await;
            let methods = server.await.unwrap();
            assert_eq!(
                methods
                    .iter()
                    .filter(|method| *method == "Page.navigate")
                    .count(),
                1,
                "history retries must not replay the navigation"
            );
            assert_eq!(
                methods
                    .iter()
                    .filter(|method| *method == "Page.getNavigationHistory")
                    .count(),
                4
            );
            assert_eq!(
                methods
                    .iter()
                    .filter(|method| *method == "Page.navigateToHistoryEntry")
                    .count(),
                usize::from(matches!(action, Some(BrowserAction::Back))),
                "back should navigate once; forward at the last entry should do nothing"
            );
        }
    }

    #[tokio::test]
    async fn screenshot_waits_for_a_frame_without_replaying_navigation() {
        let (mut chrome, server, _) = fixture("Page.captureScreenshot", Failure::Reject(3)).await;
        chrome
            .action(
                "page",
                &BrowserAction::Navigate {
                    url: "https://example.invalid/".into(),
                },
            )
            .await
            .unwrap();
        let image = tokio::time::timeout(Duration::from_secs(15), chrome.screenshot("page"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(image, [0xff, 0xd8, 0xff, 0xd9]);
        chrome.shutdown().await;
        let methods = server.await.unwrap();
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "Page.navigate")
                .count(),
            1
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "Page.captureScreenshot")
                .count(),
            4
        );
    }

    #[tokio::test]
    async fn screenshot_retries_end_without_discarding_a_healthy_transport() {
        let (mut chrome, server, _) =
            fixture("Page.captureScreenshot", Failure::Reject(usize::MAX)).await;
        let result = tokio::time::timeout(Duration::from_secs(15), chrome.screenshot("page"))
            .await
            .expect("frame retries must be bounded");
        assert!(result.is_err());
        assert!(!chrome.disconnected());
        chrome
            .call(None, "Browser.getVersion", json!({}))
            .await
            .unwrap();
        chrome.shutdown().await;
        assert!(
            server
                .await
                .unwrap()
                .iter()
                .any(|method| method == "Browser.getVersion")
        );
    }

    #[tokio::test]
    async fn uncertain_navigation_and_input_keep_the_connection_without_replaying() {
        for (method, action) in [
            (
                "Page.navigate",
                BrowserAction::Navigate {
                    url: "https://example.invalid/".into(),
                },
            ),
            (
                "Input.insertText",
                BrowserAction::Type {
                    text: "once".into(),
                },
            ),
        ] {
            let (mut chrome, server, events) = fixture(method, Failure::Defer).await;
            chrome.sessions.insert("tab".into(), "page".into());
            chrome.sizes.insert("page".into(), (390, 844));
            let error = chrome.action("page", &action).await.err().unwrap();
            assert!(error.contains("タイムアウト"));
            assert!(!chrome.disconnected());
            assert_eq!(chrome.sessions["tab"], "page");
            assert_eq!(chrome.sizes["page"], (390, 844));
            // A response arriving after its caller timed out must never answer
            // another operation or require rebuilding the connection.
            events
                .send(json!({"id":chrome.next_id,"result":{"late":true}}))
                .unwrap();
            assert_eq!(
                chrome
                    .call(None, "Browser.getVersion", json!({}))
                    .await
                    .unwrap(),
                json!({})
            );
            chrome.shutdown().await;
            let methods = server.await.unwrap();
            assert_eq!(methods.iter().filter(|sent| *sent == method).count(), 1);
        }
    }

    #[tokio::test]
    async fn idle_transport_exit_is_detected_without_a_failed_command() {
        let (chrome, server, _) = fixture("unused", Failure::Reject(0)).await;
        assert!(!chrome.disconnected());
        server.abort();
        tokio::time::timeout(Duration::from_secs(1), chrome.commands.closed())
            .await
            .unwrap();
        assert!(chrome.disconnected());
    }

    #[tokio::test]
    async fn screenshot_does_not_retry_a_disconnected_transport() {
        let (mut chrome, server, _) = fixture("Page.captureScreenshot", Failure::Disconnect).await;
        assert!(chrome.screenshot("page").await.is_err());
        assert!(chrome.disconnected());
        chrome.shutdown().await;
        assert_eq!(
            server
                .await
                .unwrap()
                .iter()
                .filter(|method| *method == "Page.captureScreenshot")
                .count(),
            1
        );
    }
}

#[cfg(test)]
fn cookies_installed(expected: &[Value], installed: &[Value]) -> bool {
    expected.iter().all(|expected| {
        installed.iter().any(|cookie| {
            cookie["session"].as_bool() == Some(expected.get("expires").is_none())
                && cookie.get("sameSite") == expected.get("sameSite")
                && expected.as_object().unwrap().iter().all(|(key, value)| {
                    if key == "expires" {
                        cookie[key].as_f64() == value.as_f64()
                    } else {
                        &cookie[key] == value
                    }
                })
        })
    })
}

#[cfg(test)]
mod cookie_tests {
    use super::{cookies_installed, json};
    #[test]
    fn verifies_scope_secrecy_lifetime_and_each_cookie_after_install() {
        let expected = json!({"domain":".example.test","name":"login","value":"fixture-login","path":"/account","httpOnly":true,"secure":true,"sameSite":"Lax","expires":1_800_000_000});
        let mut installed = expected.clone();
        installed["session"] = false.into();
        assert!(cookies_installed(
            std::slice::from_ref(&expected),
            std::slice::from_ref(&installed)
        ));
        for (key, value) in [
            ("domain", json!("example.test")),
            ("name", json!("other")),
            ("value", json!("other")),
            ("path", json!("/")),
            ("httpOnly", json!(false)),
            ("secure", json!(false)),
            ("sameSite", json!("Strict")),
            ("expires", json!(1_800_000_001)),
            ("session", json!(true)),
        ] {
            let mut changed = installed.clone();
            changed[key] = value;
            assert!(
                !cookies_installed(std::slice::from_ref(&expected), &[changed]),
                "changed {key}"
            );
        }
        assert!(!cookies_installed(std::slice::from_ref(&expected), &[]));
        let mut another = expected.clone();
        another["name"] = json!("another");
        assert!(!cookies_installed(
            &[expected.clone(), another],
            &[installed]
        ));
        let mut session = expected;
        session.as_object_mut().unwrap().remove("expires");
        session.as_object_mut().unwrap().remove("sameSite");
        let mut installed = session.clone();
        installed["session"] = true.into();
        installed["expires"] = (-1).into();
        assert!(cookies_installed(
            std::slice::from_ref(&session),
            std::slice::from_ref(&installed)
        ));
        installed["sameSite"] = json!("Strict");
        assert!(!cookies_installed(&[session], &[installed]));
    }
}
