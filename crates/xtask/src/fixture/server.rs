use super::{Config, accounts, history, scenario};
use crate::Result;
use agent_core::peer::{PeerEvent, RpcPeer, request_line};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(super) type SharedThread = Rc<RefCell<Thread>>;
pub(super) type Turn = Rc<RefCell<Value>>;

#[derive(Serialize)]
pub(super) struct Thread {
    #[serde(flatten)]
    pub metadata: Map<String, Value>,
    pub turns: Vec<Turn>,
}

impl Thread {
    fn new(id: String, cwd: Value) -> Self {
        let Value::Object(metadata) = json!({"id":id,"cwd":cwd,"name":"Simulator conversation","preview":"",
            "createdAt":1,"updatedAt":1,"status":{"type":"idle"}})
        else {
            unreachable!()
        };
        Self {
            metadata,
            turns: Vec::new(),
        }
    }
}

#[derive(Serialize)]
struct ThreadView<'a> {
    #[serde(flatten)]
    metadata: &'a Map<String, Value>,
    turns: &'a [Turn],
}

#[derive(Clone)]
struct Control {
    stop: CancellationToken,
    delayed: bool,
}

enum Output {
    Message(String),
    Barrier(oneshot::Sender<()>),
}

pub(super) struct Context {
    pub home: PathBuf,
    pub config: Config,
    output: mpsc::UnboundedSender<Output>,
    peer: Arc<RpcPeer>,
    controls: RefCell<HashMap<String, Control>>,
}

impl Context {
    fn write(&self, value: &impl Serialize) -> Result<()> {
        self.output
            .send(Output::Message(serde_json::to_string(value)?))?;
        Ok(())
    }

    pub(super) fn respond(&self, id: &Value, result: &impl Serialize) -> Result<()> {
        #[derive(Serialize)]
        struct Reply<'a, T> {
            id: &'a Value,
            result: &'a T,
        }
        self.write(&Reply { id, result })
    }

    pub(super) fn error(&self, id: &Value, code: i32, message: &str) -> Result<()> {
        self.write(&json!({"id":id,"error":{"code":code,"message":message}}))
    }

    pub fn notify(&self, method: &str, params: &impl Serialize) -> Result<()> {
        #[derive(Serialize)]
        struct Notification<'a, T> {
            method: &'a str,
            params: &'a T,
        }
        self.write(&Notification { method, params })
    }

    pub fn request<'a>(
        &'a self,
        method: &str,
        params: &Value,
    ) -> Result<(
        u64,
        impl std::future::Future<Output = Result<Value>> + use<'a>,
    )> {
        let reply = self.peer.request_raw(&request_line(method, params)?);
        let id = reply
            .wire_id()
            .ok_or("fixture request preparation failed")?;
        let (flushed, ready) = oneshot::channel();
        self.output.send(Output::Barrier(flushed))?;
        Ok((id, async move {
            ready.await?;
            let mut response: Value = serde_json::from_str(&reply.await?.value)?;
            Ok(response["result"].take())
        }))
    }

    pub fn item_event(
        &self,
        method: &str,
        thread_id: &str,
        turn_id: &str,
        item: &Value,
    ) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ItemEvent<'a> {
            thread_id: &'a str,
            turn_id: &'a str,
            item: &'a Value,
        }
        self.notify(
            method,
            &ItemEvent {
                thread_id,
                turn_id,
                item,
            },
        )
    }

    pub fn stream_item(&self, thread_id: &str, turn: &Turn, item: Value) -> Result<()> {
        let mut turn = turn.borrow_mut();
        self.item_event(
            "item/started",
            thread_id,
            turn["id"].as_str().unwrap(),
            &item,
        )?;
        turn["items"].as_array_mut().unwrap().push(item);
        Ok(())
    }

    pub fn turn_event(&self, method: &str, thread_id: &str, turn: &Value) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct TurnEvent<'a> {
            thread_id: &'a str,
            turn: &'a Value,
        }
        self.notify(method, &TurnEvent { thread_id, turn })
    }

    pub fn finish(
        &self,
        thread: &SharedThread,
        turn: &Turn,
        status: &str,
        error: Option<Value>,
    ) -> Result<()> {
        let mut thread = thread.borrow_mut();
        let mut turn = turn.borrow_mut();
        turn["status"] = status.into();
        turn["completedAt"] = 4.into();
        turn["durationMs"] = 3000.into();
        if let Some(error) = error {
            turn["error"] = error;
        }
        thread
            .metadata
            .insert("status".into(), json!({"type":"idle"}));
        let id = thread.metadata["id"].as_str().unwrap();
        self.turn_event("turn/completed", id, &turn)?;
        self.notify(
            "thread/status/changed",
            &json!({"threadId":id,"status":{"type":"idle"}}),
        )
    }

    fn trace(&self, method: &str, facts: Value) -> Result<()> {
        if !self.config.trace {
            return Ok(());
        }
        let mut facts = facts;
        facts["method"] = method.into();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.home.join("rpc-trace.jsonl"))?;
        serde_json::to_writer(&mut file, &facts)?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

pub(super) async fn run(home: PathBuf, config: Config) -> Result<()> {
    tokio::task::LocalSet::new().run_until(async move {
        let peer = Arc::new(RpcPeer::open(agent_core::peer::JsonlReader::new(tokio::io::stdin()), tokio::io::stdout(),
            None, 1024)?);
        let mut lines = peer.subscribe();
        let (output, mut outbound) = mpsc::unbounded_channel();
        let writer_peer = peer.clone();
        let mut writer = tokio::spawn(async move {
            while let Some(output) = outbound.recv().await {
                match output {
                    Output::Message(line) => writer_peer.send_raw(line).await?,
                    Output::Barrier(ready) => { let _ = ready.send(()); }
                }
            }
            Ok::<_, agent_core::peer::PeerError>(())
        });
        let context = Rc::new(Context { home, config, output, peer: peer.clone(), controls: RefCell::new(HashMap::new()) });
        let result = async {
        let mut accounts = accounts::Accounts::load(&context.home)?;
        let mut threads = IndexMap::<String, SharedThread>::new();
        let mut saved_threads = None;
        let mut list_contents = None;
        let mut next_thread = 0;
        loop {
            let line = tokio::select! {
                result = &mut writer => { result??; break; }
                line = lines.recv() => match line {
                    Ok(PeerEvent::Message(message)) => message.value,
                    Ok(PeerEvent::Response { .. }) => continue,
                    Ok(PeerEvent::Closed(_)) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(error) => return Err(error.into()),
                }
            };
            let mut message: Value = serde_json::from_str(&line)?;
            let mut owned_params = message["params"].take();
            let Some(id) = message.get("id") else { continue };
            let method = message["method"].as_str().unwrap_or("");
            let params = &mut owned_params;
            match method {
                "initialize" => context.respond(id, &json!({"userAgent":"remote-agent-simulator-fixture",
                    "platformFamily":"unix","platformOs":"macos","codexHome":context.home}))?,
                "account/read" | "getAuthStatus" | "account/login/start" | "account/login/cancel" | "account/logout" | "fixture/account/current" | "fixture/account/refresh" => accounts.request(&context, id, method, params)?,
                "model/list" => {
                    let path = context.home.join("models-fixture.json");
                    let models = if path.exists() { serde_json::from_slice(&fs::read(path)?)? } else {
                        json!([{"id":"fixture-model","model":"fixture-model","displayName":"Fixture Model",
                            "defaultReasoningEffort":"medium","supportedReasoningEfforts":[
                                {"reasoningEffort":"medium","description":"Balanced"},
                                {"reasoningEffort":"high","description":"Detailed"}],
                            "isDefault":true,"hidden":false,"description":"Isolated test model"}])
                    };
                    context.respond(id, &json!({"data":models,"nextCursor":null}))?;
                }
                "thread/list" => {
                    if context.home.join("exit-on-list").exists() { std::process::exit(0); }
                    let fixture = context.home.join("list-fixture.json");
                    let contents = if fixture.exists() { Some(fs::read(&fixture)?) } else { None };
                    if contents.is_some() && contents != list_contents {
                        if saved_threads.is_none() { saved_threads = Some(std::mem::take(&mut threads)); }
                        list_contents = contents;
                        threads.clear();
                        let summaries: Vec<Map<String, Value>> = serde_json::from_slice(list_contents.as_ref().unwrap())?;
                        for mut summary in summaries {
                            let thread_id = summary["id"].as_str().ok_or("fixture thread lacks id")?.to_owned();
                            let mut thread = Thread::new(thread_id.clone(), summary.remove("cwd").unwrap_or(Value::Null));
                            thread.metadata.extend(summary);
                            thread.turns.push(Rc::new(RefCell::new(json!({"id":format!("turn-{thread_id}"),"status":"completed","items":[
                                {"id":format!("answer-{thread_id}"),"type":"agentMessage","phase":"final_answer",
                                    "text":format!("History for {}",thread.metadata["name"].as_str().unwrap_or(""))}]}))));
                            threads.insert(thread_id, Rc::new(RefCell::new(thread)));
                        }
                    } else if contents.is_none() && let Some(saved) = saved_threads.take() {
                        threads = saved; list_contents = None;
                    }
                    if fixture.exists() && params["useStateDbOnly"] != true {
                        context.error(id, -32602, "Title lists must not scan rollout history")?;
                        continue;
                    }
                    let term = params["searchTerm"].as_str().unwrap_or("").to_lowercase();
                    let mut ordered: Vec<_> = threads.values().map(|thread| thread.borrow()).filter(|thread| {
                        term.is_empty() || thread.metadata.get("name").and_then(Value::as_str).filter(|name| !name.is_empty())
                            .or_else(|| thread.metadata.get("preview").and_then(Value::as_str)).unwrap_or("").to_lowercase().contains(&term)
                    }).collect();
                    ordered.sort_by_key(|thread| std::cmp::Reverse(thread.metadata["updatedAt"].as_i64().unwrap_or(0)));
                    let offset = offset(params);
                    let end = offset.saturating_add(limit(params, 64).min(100));
                    let page: Vec<_> = ordered.iter().skip(offset).take(end - offset)
                        .map(|thread| ThreadView { metadata: &thread.metadata, turns: &[] }).collect();
                    context.trace(method, json!({"useStateDbOnly":params["useStateDbOnly"] == true,"count":page.len()}))?;
                    #[derive(Serialize)] #[serde(rename_all = "camelCase")]
                    struct Page<'a> { data: &'a [ThreadView<'a>], next_cursor: Option<String> }
                    context.respond(id, &Page { data: &page, next_cursor: (end < ordered.len()).then(|| end.to_string()) })?;
                }
                "thread/start" => {
                    let cwd = match params.get_mut("cwd") {
                        Some(cwd) => cwd.take(),
                        // Unscoped chats must never inherit the developer's
                        // checkout through the fixture process environment.
                        None => serde_json::to_value(&context.home)?,
                    };
                    let matches = context.config.expected_cwd.as_ref().is_none_or(|expected| cwd.as_str().is_some_and(|cwd| std::path::Path::new(cwd) == expected));
                    context.trace(method, json!({"hasProjectId":params.get("projectId").is_some(),"cwdMatchesFixture":matches}))?;
                    if params.get("projectId").is_some() { context.error(id, -32600, "project not found: desktop-project")?; continue; }
                    if !matches || !cwd.as_str().is_some_and(|cwd| !cwd.is_empty()) {
                        context.error(id, -32602, "invalid cwd")?; continue;
                    }
                    next_thread += 1;
                    let thread_id = format!("fixture-thread-{next_thread}");
                    let mut thread = Thread::new(thread_id.clone(), cwd);
                    thread.metadata.insert("path".into(), context.home.join(format!("{thread_id}.jsonl")).into_os_string().into_string().unwrap().into());
                    thread.metadata.insert("historyMode".into(), "paginated".into());
                    thread.metadata.insert("model".into(), params["model"].take());
                    thread.metadata.insert("createdAt".into(), next_thread.into());
                    thread.metadata.insert("updatedAt".into(), next_thread.into());
                    #[derive(Serialize)] struct Started<'a> { thread: &'a Thread }
                    context.respond(id, &Started { thread: &thread })?;
                    threads.insert(thread_id, Rc::new(RefCell::new(thread)));
                }
                "thread/fork" => {
                    let Some(source) = threads.get(params["threadId"].as_str().unwrap_or("")).cloned() else {
                        context.error(id, -32602, "thread not found")?; continue;
                    };
                    let source = source.borrow();
                    let Some(boundary) = source.turns.iter().position(|turn| turn.borrow()["id"] == params["lastTurnId"]) else {
                        context.error(id, -32602, "turn not found")?; continue;
                    };
                    if source.turns[boundary].borrow()["status"] == "inProgress" {
                        context.error(id, -32602, "completed turn required")?; continue;
                    }
                    next_thread += 1;
                    let thread_id = format!("fixture-thread-{next_thread}");
                    let mut thread = Thread { metadata: source.metadata.clone(),
                        turns: source.turns[..=boundary].iter().map(|turn| Rc::new(RefCell::new(turn.borrow().clone()))).collect() };
                    thread.metadata.insert("id".into(), thread_id.clone().into());
                    thread.metadata.insert("createdAt".into(), next_thread.into());
                    thread.metadata.insert("updatedAt".into(), next_thread.into());
                    thread.metadata.insert("status".into(), json!({"type":"idle"}));
                    #[derive(Serialize)] struct Forked<'a> { thread: ThreadView<'a> }
                    context.respond(id, &Forked { thread: ThreadView { metadata: &thread.metadata,
                        turns: if params["excludeTurns"] == true { &[] } else { &thread.turns } } })?;
                    threads.insert(thread_id, Rc::new(RefCell::new(thread)));
                }
                "turn/start" => {
                    let thread_id = params["threadId"].as_str().unwrap_or("");
                    let prompt = params["input"][0]["text"].as_str().unwrap_or("");
                    let has_text = params["input"][0]["type"] == "text" && !prompt.trim().is_empty();
                    context.trace(method, json!({"threadExists":threads.contains_key(thread_id),"hasTextInput":has_text,
                        "model":params["model"],"effort":params["effort"],"serviceTierForTurn":params["serviceTierForTurn"]}))?;
                    let Some(thread) = threads.get(thread_id).filter(|_| has_text) else {
                        context.error(id, -32602, "invalid turn input")?; continue;
                    };
                    if prompt.contains("[model]") && (params["model"] != "fixture-model" || params["effort"] != "high" || thread.borrow().metadata.get("model").and_then(Value::as_str) != Some("fixture-model")) {
                        context.error(id, -32602, "selected model and effort did not reach the server")?; continue;
                    }
                    if thread.borrow().turns.iter().any(|turn| turn.borrow()["status"] == "inProgress") {
                        context.error(id, -32600, "turn already active")?; continue;
                    }
                    let suffix = format!("{}-{}", thread_id.rsplit('-').next().unwrap(), thread.borrow().turns.len() + 1);
                    let turn_id = format!("fixture-turn-{suffix}");
                    let turn = Rc::new(RefCell::new(json!({"id":turn_id,"status":"inProgress","items":[],"startedAt":1})));
                    thread.borrow_mut().turns.push(turn.clone());
                    thread.borrow_mut().metadata.insert("status".into(), json!({"type":"active","activeFlags":[]}));
                    context.respond(id, &json!({"turn":{"id":turn_id}}))?;
                    let delayed = prompt.contains("[delayed-input]") || prompt.contains("[deferred-steer]");
                    if delayed || prompt.contains("[workspace-edit]") { remove_if_present(&context.home.join("release-inputs"))?; }
                    let stop = CancellationToken::new();
                    context.controls.borrow_mut().insert(turn_id, Control { stop: stop.clone(), delayed });
                    let context = context.clone();
                    let thread = thread.clone();
                    let input = std::mem::take(params["input"].as_array_mut().unwrap());
                    let client_id = params["clientUserMessageId"].take();
                    tokio::task::spawn_local(async move {
                        if let Err(error) = scenario::run(context.clone(), thread.clone(), turn.clone(), input, suffix, stop, client_id).await {
                            eprintln!("Codex scenario failed: {error}");
                            let _ = context.finish(&thread, &turn, "failed", Some(json!({"message":"fixture scenario failed"})));
                        }
                    });
                }
                "turn/interrupt" | "turn/steer" => {
                    let thread_id = params["threadId"].as_str().unwrap_or("");
                    let active = threads.get(thread_id).and_then(|thread| thread.borrow().turns.iter()
                        .find(|turn| turn.borrow()["status"] == "inProgress").cloned());
                    let expected = &params[if method == "turn/interrupt" { "turnId" } else { "expectedTurnId" }];
                    let Some(turn) = active.filter(|turn| &turn.borrow()["id"] == expected) else {
                        context.error(id, -32602, "active turn changed")?; continue;
                    };
                    let turn_id = turn.borrow()["id"].as_str().unwrap().to_owned();
                    let control = context.controls.borrow()[&turn_id].clone();
                    if method == "turn/interrupt" {
                        control.stop.cancel(); context.respond(id, &json!({}))?;
                    } else {
                        let item_id = params["clientUserMessageId"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)
                            .unwrap_or_else(|| turn.borrow()["items"].as_array().unwrap().len().to_string());
                        let item = json!({"id":format!("steered-{item_id}"),"clientId":params["clientUserMessageId"],"type":"userMessage",
                            "content":params.get("input").unwrap_or(&json!([]))});
                        if control.delayed {
                            let context = context.clone(); let thread_id = thread_id.to_owned();
                            tokio::task::spawn_local(async move {
                                if scenario::wait_for_release(&context.home, &control.stop).await {
                                    let recorded = json!({"id":format!("fixture-steer-recorded-{}",item["id"].as_str().unwrap()),"type":"agentMessage","phase":"commentary","text":"Additional input recorded"});
                                    if context.stream_item(&thread_id, &turn, item).and_then(|_| context.stream_item(&thread_id, &turn, recorded)).is_err() {
                                        eprintln!("Deferred fixture input could not be written");
                                    }
                                }
                            });
                        } else { context.stream_item(thread_id, &turn, item)?; }
                        context.respond(id, &json!({"turnId":turn_id}))?;
                    }
                }
                "thread/turns/list" | "thread/items/list" => {
                    let Some(thread) = threads.get(params["threadId"].as_str().unwrap_or("")) else {
                        context.error(id, -32602, "thread not found")?; continue;
                    };
                    history::page(&context, id, &thread.borrow(), method, params)?;
                }
                "thread/read" | "thread/resume" => {
                    let failure = context.home.join("fail-next-history-read");
                    if method == "thread/read" && failure.exists() {
                        fs::remove_file(failure)?;
                        context.error(id, -32601, "list_turns is not supported yet")?;
                        continue;
                    }
                    let external = context.home.join("background-reply");
                    if external.exists()
                        && let Some(latest) = threads.values().max_by_key(|thread| thread.borrow().metadata["createdAt"].as_i64().unwrap_or(0)) {
                            latest.borrow_mut().turns.push(Rc::new(RefCell::new(json!({"id":"fixture-external-turn","status":"completed","items":[
                                {"id":"fixture-external-final","type":"agentMessage","phase":"final_answer","text":fs::read_to_string(&external)?}]}))));
                            fs::remove_file(external)?;
                    }
                    let Some(thread) = threads.get(params["threadId"].as_str().unwrap_or("")) else {
                        context.error(id, -32602, "thread not found")?; continue;
                    };
                    let stored_thread = thread.borrow();
                    let overridden = stored_thread.metadata.get("readCwd").map(|cwd| {
                        let mut metadata = stored_thread.metadata.clone();
                        metadata.insert("cwd".into(), cwd.clone());
                        let notification = json!({"threadId":metadata["id"], "threadName":metadata["name"]});
                        let context = context.clone();
                        tokio::task::spawn_local(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            context.notify("thread/name/updated", &notification)
                        });
                        Thread { metadata, turns: stored_thread.turns.clone() }
                    });
                    let thread = overridden.as_ref().unwrap_or(&stored_thread);
                    let include = params["includeTurns"] == true;
                    if method == "thread/read" && include && thread.metadata.get("historyMode").and_then(Value::as_str) == Some("paginated") {
                        context.error(id, -32603, "Full history hydration is unavailable; use pagination")?;
                    } else if method == "thread/read" && include && !thread.turns.is_empty() && thread.turns.iter().all(|turn| {
                        let turn = turn.borrow(); turn["status"] == "inProgress" && !turn["items"].as_array().unwrap().iter().any(|item| item["type"] == "agentMessage")
                    }) { context.error(id, -32603, "rollout is empty")?;
                    } else if method == "thread/resume" && thread.turns.is_empty() {
                        context.error(id, -32600, "no rollout found for thread id")?;
                    } else {
                        #[derive(Serialize)] struct Read<'a, T> { thread: &'a T }
                        if include {
                            if let Some(persisted) = history::persisted(thread) {
                                context.respond(id, &Read { thread: &persisted })?;
                            } else { context.respond(id, &Read { thread })?; }
                        } else {
                            context.respond(id, &Read { thread: &ThreadView { metadata: &thread.metadata,
                                turns: if method == "thread/read" { &[] } else { &thread.turns } } })?;
                        }
                    }
                }
                _ => context.error(id, -32601, "method not found")?,
            }
        }
        Ok(())
        }.await;
        for control in context.controls.borrow().values() { control.stop.cancel(); }
        let closed = peer.close().await;
        writer.abort();
        result.and(closed.map_err(Into::into))
    }).await
}

pub(super) fn offset(params: &Value) -> usize {
    params["cursor"]
        .as_str()
        .and_then(|cursor| cursor.parse().ok())
        .unwrap_or(0)
}

pub(super) fn limit(params: &Value, default: usize) -> usize {
    params["limit"]
        .as_u64()
        .filter(|limit| *limit > 0)
        .map(|limit| limit as usize)
        .unwrap_or(default)
}

pub(super) fn remove_if_present(path: &std::path::Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
