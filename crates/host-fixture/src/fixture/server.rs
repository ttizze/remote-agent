use super::{Config, accounts, history, scenario};
use crate::Result;
use agent_transport::peer::{PeerEvent, RpcPeer, request_line};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::fs;
use std::{
    cell::RefCell, collections::HashMap, fs::OpenOptions, io::Write, path::PathBuf, rc::Rc,
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

fn projects(home: &std::path::Path) -> crate::Result<Vec<Value>> {
    match fs::read(home.join("projects.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

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

    fn respond_history(&self, id: &Value, result: &impl Serialize) -> Result<()> {
        let gate = self.home.join("hold-history-reads");
        if !gate.exists() {
            return self.respond(id, result);
        }
        let response = serde_json::to_string(&json!({"id":id,"result":result}))?;
        let id = id.clone();
        let output = self.output.clone();
        fs::write(self.home.join("history-read-held"), [])?;
        tokio::spawn(async move {
            let released = tokio::time::timeout(std::time::Duration::from_secs(25), async {
                while gate.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await;
            let response = if released.is_ok() {
                response
            } else {
                json!({"id":id,"error":{"code":-32000,"message":"fixture history gate timed out"}})
                    .to_string()
            };
            let _ = output.send(Output::Message(response));
        });
        Ok(())
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
        turn["completedAt"] = if self.config.live_clock {
            (turn["startedAt"].as_f64().unwrap_or(1.) + 3.).into()
        } else {
            4.into()
        };
        turn["durationMs"] = 3000.into();
        if let Some(error) = error {
            turn["error"] = error;
        }
        thread
            .metadata
            .insert("status".into(), json!({"type":"idle"}));
        if self.config.deferred_thread_metadata {
            thread
                .metadata
                .insert("name".into(), "Completed conversation".into());
        }
        let id = thread.metadata["id"].as_str().unwrap();
        self.turn_event("turn/completed", id, &turn)?;
        self.notify(
            "thread/status/changed",
            &json!({"threadId":id,"status":{"type":"idle"}}),
        )
    }

    pub(super) fn trace(&self, method: &str, facts: Value) -> Result<()> {
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
        let peer = Arc::new(RpcPeer::open(agent_transport::peer::JsonlReader::new(tokio::io::stdin()), tokio::io::stdout(),
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
            Ok::<_, agent_transport::peer::PeerError>(())
        });
        let context = Rc::new(Context { home, config, output, peer: peer.clone(), controls: RefCell::new(HashMap::new()) });
        let result = async {
        let mut accounts = accounts::Accounts::load(&context.home)?;
        let mut threads = IndexMap::<String, SharedThread>::new();
        let mut saved_threads = None;
        let mut list_contents = None;
        let mut next_thread = 0;
        // Only the provider's process lifecycle protocol is modeled here. No
        // shell commands are executed by this deterministic Codex double.
        let mut processes = std::collections::HashSet::<String>::new();
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
                "process/spawn" => {
                    let handle = params["processHandle"].as_str().unwrap_or("");
                    if handle.is_empty() || !params["cwd"].as_str().is_some_and(|cwd| std::path::Path::new(cwd).is_dir())
                        || !params["command"].as_array().is_some_and(|command| !command.is_empty())
                        || processes.contains(handle)
                    {
                        context.error(id, -32602, "invalid process start")?;
                    } else {
                        processes.insert(handle.to_owned());
                        context.respond(id, &json!({}))?;
                    }
                }
                "process/kill" => {
                    let handle = params["processHandle"].as_str().unwrap_or("");
                    if processes.remove(handle) {
                        context.notify("process/exited", &json!({"processHandle":handle,"exitCode":0}))?;
                        context.respond(id, &json!({}))?;
                    } else { context.error(id, -32602, "process not found")?; }
                }
                "initialize" => {
                    if params["capabilities"]["experimentalApi"] != true {
                        context.error(id, -32602, "experimentalApi capability required")?;
                        continue;
                    }
                    if let Some(gate) = &context.config.initialize_gate {
                        fs::write(gate.with_extension("entered"), [])?;
                        tokio::time::timeout(std::time::Duration::from_secs(10), async {
                            while !gate.exists() {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                        }).await?;
                    }
                    context.respond(id, &json!({"userAgent":"remote-agent-simulator-fixture",
                        "platformFamily":"unix","platformOs":"macos","codexHome":context.home}))?;
                },
                "account/read" | "account/rateLimits/read" | "getAuthStatus" | "account/login/start" | "account/login/cancel" | "account/logout" | "fixture/account/current" | "fixture/account/refresh" => accounts.request(&context, id, method, params)?,
                "skills/list" => {
                    context.respond(id, &json!({"data":[{"cwd":params["cwds"][0],"skills":[
                        {"name":"fixture-review","path":"/fixture/skills/review/SKILL.md","description":"Review fixture changes","enabled":true},
                        {"name":"disabled-skill","path":"/fixture/disabled/SKILL.md","description":"Disabled","enabled":false}
                    ],"errors":[]}]}))?;
                }
                "plugin/list" => {
                    context.respond(id, &json!({"marketplaces":[{"plugins":[
                        {"id":"fixture@local","name":"fixture","installed":true,"enabled":true,"availability":"AVAILABLE","interface":{"displayName":"Fixture Plugin","shortDescription":"Fixture tools"}},
                        {"id":"disabled@local","name":"disabled","installed":true,"enabled":false,"availability":"AVAILABLE","interface":null},
                        {"id":"uninstalled@local","name":"uninstalled","installed":false,"enabled":true,"availability":"AVAILABLE","interface":null},
                        {"id":"blocked@local","name":"blocked","installed":true,"enabled":true,"availability":"DISABLED_BY_ADMIN","interface":null}
                    ]}],"marketplaceLoadErrors":[]}))?;
                }
                "config/read" | "config/batchWrite" => {
                    let path = context.home.join("fixture-native-config.json");
                    let bytes = fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
                    let mut config: Value = serde_json::from_str(&bytes)?;
                    if method == "config/read" {
                        context.respond(id, &json!({"config":config,"origins":{},"layers":[{
                            "name":{"type":"user","file":path},"config":config,"version":bytes
                        }]}))?;
                    } else if params["expectedVersion"] != bytes || params["filePath"] != json!(path) || params["reloadUserConfig"] != false {
                        context.error(id, -32602, "incorrect config write preconditions")?;
                    } else {
                        for edit in params["edits"].as_array().unwrap() {
                            assert_eq!(edit["mergeStrategy"], "replace");
                            config[edit["keyPath"].as_str().unwrap()] = edit["value"].clone();
                        }
                        let bytes = serde_json::to_string(&config)?;
                        fs::write(&path, &bytes)?;
                        context.respond(id, &json!({"filePath":path,"version":bytes,"status":"ok"}))?;
                    }
                }
                "model/list" => {
                    let models = json!([{"id":"fixture-model","model":"fixture-model","displayName":"Fixture Model",
                            "defaultServiceTier":"default","serviceTiers":[{"id":"priority","name":"高速"}],
                            "defaultReasoningEffort":"medium","supportedReasoningEfforts":[
                                {"reasoningEffort":"medium","description":"Balanced"},
                                {"reasoningEffort":"high","description":"Detailed"}],
                            "isDefault":true,"hidden":false,"description":"Isolated test model"}]);
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
                            let persisted_turns = summary.remove("turns");
                            thread.metadata.extend(summary);
                            thread.turns.push(Rc::new(RefCell::new(json!({"id":format!("turn-{thread_id}"),"status":"completed","items":[
                                {"id":format!("answer-{thread_id}"),"type":"agentMessage","phase":"final_answer",
                                    "text":format!("History for {}",thread.metadata["name"].as_str().unwrap_or(""))}]}))));
                            if let Some(turns) = persisted_turns {
                                thread.turns = serde_json::from_value::<Vec<Value>>(turns)?
                                    .into_iter().map(|turn| Rc::new(RefCell::new(turn))).collect();
                            }
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
                        !context.config.deferred_thread_metadata || thread.turns.iter().any(|turn| turn.borrow()["status"] != "inProgress")
                    }).filter(|thread| {
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
                "project/list" => {
                    let mut projects = projects(&context.home)?;
                    for (index, project) in projects.iter_mut().enumerate() {
                        let fields = project.as_object_mut().ok_or("fixture project must be an object")?;
                        fields.entry("createdAt").or_insert(json!(0));
                        fields.entry("updatedAt").or_insert(json!(0));
                        fields.entry("position").or_insert(json!(index));
                        fields.entry("metadata").or_insert(json!({}));
                    }
                    let offset = offset(params);
                    let end = offset.saturating_add(limit(params, 100));
                    context.respond(id, &json!({"data":projects.iter().skip(offset).take(end-offset).collect::<Vec<_>>(),
                        "nextCursor":(end < projects.len()).then(|| end.to_string())}))?;
                }
                "project/create" => {
                    let mut projects = projects(&context.home)?;
                    let project = if let Some(project) = projects.iter().find(|project| project["metadata"]["fixtureIdempotencyKey"] == params["idempotencyKey"]) {
                        project.clone()
                    } else {
                        let project = json!({"id":format!("fixture-project-{}", projects.len()+1),"name":params["name"],
                            "roots":params["roots"],"metadata":{"fixtureIdempotencyKey":params["idempotencyKey"]},
                            "position":projects.len(),"createdAt":1,"updatedAt":1});
                        projects.push(project.clone());
                        fs::write(context.home.join("projects.json"), serde_json::to_vec(&projects)?)?;
                        project
                    };
                    context.respond(id, &json!({"project":project}))?;
                }
                "thread/start" => {
                    let failure = context.home.join("fail-next-thread-start");
                    if failure.exists() {
                        fs::remove_file(failure)?;
                        context.error(id, -32603, "fixture session creation failed")?;
                        continue;
                    }
                    let cwd = match params.get_mut("cwd") {
                        Some(cwd) => cwd.take(),
                        // Unscoped chats must never inherit the developer's
                        // checkout through the fixture process environment.
                        None => serde_json::to_value(&context.home)?,
                    };
                    let matches = context.config.expected_cwd.as_ref().is_none_or(|expected| cwd.as_str().is_some_and(|cwd| std::path::Path::new(cwd) == expected));
                    context.trace(method, json!({"hasProjectId":params.get("projectId").is_some(),"cwdMatchesFixture":matches}))?;
                    if let Some(project_id) = params.get("projectId").filter(|id| !id.is_null())
                        && !projects(&context.home)?.iter().any(|project| &project["id"] == project_id)
                    { context.error(id, -32600, "project not found")?; continue; }
                    if !matches || !cwd.as_str().is_some_and(|cwd| !cwd.is_empty()) {
                        context.error(id, -32602, "invalid cwd")?; continue;
                    }
                    next_thread += 1;
                    let thread_id = format!("fixture-thread-{next_thread}");
                    let mut thread = Thread::new(thread_id.clone(), cwd);
                    if let Some(project_id) = params.get("projectId") { thread.metadata.insert("projectId".into(), project_id.clone()); }
                    if context.config.deferred_thread_metadata { thread.metadata.insert("name".into(), Value::Null); }
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
                    let started = if context.config.live_clock {
                        json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs_f64())
                    } else { json!(1) };
                    let turn = Rc::new(RefCell::new(json!({"id":turn_id,"status":"inProgress","items":[],"startedAt":started})));
                    thread.borrow_mut().turns.push(turn.clone());
                    thread.borrow_mut().metadata.insert("status".into(), json!({"type":"active","activeFlags":[]}));
                    context.respond(id, &json!({"turn":{"id":turn_id}}))?;
                    let delayed = prompt.contains("[delayed-input]") || prompt.contains("[deferred-steer]");
                    if delayed || prompt.contains("[workspace-edit]") || prompt.contains("[groups]") { remove_if_present(&context.home.join("release-inputs"))?; }
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
                "thread/turns/list" | "thread/items/list" | "thread/timeline/list" => {
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
                            let response = Read { thread: &ThreadView { metadata: &thread.metadata,
                                turns: if method == "thread/read" { &[] } else { &thread.turns } } };
                            if method == "thread/read" { context.respond_history(id, &response)?; }
                            else { context.respond(id, &response)?; }
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
