//! Every agent-providers reference transcript through the store, actors, outbox,
//! executors and session manager. A fake process checks each `expect_outbound`
//! frame and plays each `emit_inbound` frame; the projections must meet the
//! expectations of the reference tests.
//!
//! The graph transcripts (`thread_fork_native*`, `thread_merge_back*`,
//! `thread_rollback*`, `delegated_task_status`) drive several app threads: Codex
//! threads share one app-server and JSON-RPC id space, and each Claude thread
//! runs its own CLI. A recorded Claude fork's session id maps to the one the
//! Host writes. The 0.137 Codex rollouts predate history injection, so, as the
//! reference harness does, `thread/inject_items` succeeds outside the
//! recording: its request ids are skipped in the recorded id space and the
//! recorded inline handoff becomes the user's text.
use super::fake::FakeHost;
use super::{Rig, runtime_rig as rig};
use crate::{DaemonOptions, EffectDaemon, SessionOptions};
use agent_domain::*;
use agent_providers::ClaudeLaunch;
use agent_providers::replay_support::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn optional(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

const GRAPH: [&str; 4] = [
    "thread_fork_native",
    "thread_merge_back",
    "thread_rollback",
    "delegated_task_status",
];
fn graph(scenario: &str) -> bool {
    GRAPH.iter().any(|prefix| scenario.starts_with(prefix))
}

/// The SDK answers its own `initialize`; the recorder does not show it.
fn claude_initialize(frame: &Value) -> Vec<Value> {
    if frame["type"] == "control_request" && frame["request"]["subtype"] == "initialize" {
        return vec![
            json!({"type":"control_response","response":{"subtype":"success","request_id":frame["request_id"],"response":{}}}),
        ];
    }
    vec![]
}

/// The 0.137 rollouts predate `thread/inject_items`.
fn codex_frame(frame: &Value) -> Option<Value> {
    (frame["method"] != "thread/inject_items").then(|| frame.clone())
}

/// Recorded Claude session -> the session the runtime created for it.
type Sessions = Arc<Mutex<BTreeMap<String, String>>>;

/// The recorded launch options a new Claude process must match.
fn check_open(expected: &Value, launch: &ClaudeLaunch, sessions: &Sessions) -> Result<(), String> {
    let args = launch.args();
    let value = |name: &str| {
        args.iter()
            .find_map(|arg| arg.strip_prefix(&format!("--{name}=")).map(str::to_owned))
            .or_else(|| {
                args.iter()
                    .position(|arg| arg == &format!("--{name}"))
                    .map(|index| args[index + 1].clone())
            })
    };
    let options = &expected["options"];
    let settings =
        value("settings").map(|settings| serde_json::from_str::<Value>(&settings).unwrap());
    let resume = options["resume"].as_str().map(|recorded| {
        sessions
            .lock()
            .unwrap()
            .get(recorded)
            .cloned()
            .unwrap_or_else(|| recorded.to_owned())
    });
    let mut mismatches = vec![];
    if value("model").as_deref() != options["model"].as_str() {
        mismatches.push(format!("model {:?}", value("model")));
    }
    if settings != Some(options["settings"].clone()) {
        mismatches.push(format!("settings {settings:?}"));
    }
    if value("resume") != resume {
        mismatches.push(format!("resume {:?}", value("resume")));
    }
    if options["resume"].is_string()
        && value("resume-session-at").as_deref() != options["resumeSessionAt"].as_str()
    {
        mismatches.push(format!(
            "resume-session-at {:?}",
            value("resume-session-at")
        ));
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{expected} launched with {}",
            mismatches.join(", ")
        ))
    }
}

/// The app threads a graph transcript created, in order.
#[derive(Default)]
struct Graph {
    forks: Vec<ThreadId>,
    merges: usize,
    /// The thread of the latest recorded Claude query.
    current: Option<ThreadId>,
    /// A Claude fork waiting for its recorded `session.forked`.
    forking: Option<ThreadId>,
    /// A Claude fork whose recorded session the child's first message opens.
    unconsumed: Option<(String, ThreadId)>,
    delegated: Option<ThreadId>,
}

struct Replay {
    rig: Rig,
    _daemon: EffectDaemon,
    driver: Driver,
    scenario: String,
    root: ThreadId,
    rows: Vec<Value>,
    opens: Arc<Mutex<VecDeque<Value>>>,
    errors: Arc<Mutex<Vec<String>>>,
    ignored_config: Vec<String>,
    sessions: Sessions,
    graph: Mutex<Graph>,
    /// History the runtime offered through `thread/inject_items`.
    injected: Arc<Mutex<Vec<Value>>>,
}

impl Replay {
    async fn start(scenario: &str, driver: Driver) -> Self {
        let rows = transcript(scenario, driver);
        let rig = rig(
            SessionOptions {
                idle_timeout: Duration::from_secs(3600),
                ..SessionOptions::default()
            },
            5,
        );
        let host: &FakeHost = &rig.host;
        let opens = Arc::new(Mutex::new(VecDeque::new()));
        let errors = Arc::new(Mutex::new(vec![]));
        let sessions = Sessions::default();
        let injected = Arc::new(Mutex::new(vec![]));
        *host.prompts.lock().unwrap() = rows
            .iter()
            .filter(|row| {
                row["type"] == "expect_outbound" && row["frame"]["type"] == "prompt.offer"
            })
            .enumerate()
            .map(|(i, row)| {
                optional(&row["frame"]["message"], "uuid").unwrap_or_else(|| format!("prompt-{i}"))
            })
            .collect();
        if driver == Driver::Claude {
            *host.translate.lock().unwrap() = Some(sdk_frame);
            host.respond(claude_initialize);
            let (opens, errors, sessions) = (opens.clone(), errors.clone(), sessions.clone());
            host.before_spawn(move |request| {
                let result = match (request.claude.as_ref(), opens.lock().unwrap().pop_front()) {
                    (Some(launch), Some(expected)) => check_open(&expected, launch, &sessions),
                    (_, None) => {
                        Err("a Claude process opened without a recorded query.open".into())
                    }
                    (None, _) => Err("a Codex process opened in a Claude transcript".into()),
                };
                if let Err(error) = result {
                    errors.lock().unwrap().push(error);
                }
                Box::pin(async { Ok(()) })
            });
        } else if graph(scenario) {
            *host.translate.lock().unwrap() = Some(codex_frame);
            let injected = injected.clone();
            host.respond(move |frame| {
                if frame["method"] != "thread/inject_items" {
                    return vec![];
                }
                injected.lock().unwrap().push(frame.clone());
                vec![json!({"id":frame["id"],"result":{}})]
            });
        }
        {
            let mut context = host.context.lock().unwrap();
            context.cwd = "<workspace>".into();
            context.client_version = "<ignored>".into();
            // The recorder's explicit runtime policy is an input of the turn.
            if let Some(start) = rows.iter().find(|row| {
                row["type"] == "expect_outbound" && row["frame"]["method"] == "turn/start"
            }) {
                let params = &start["frame"]["params"];
                if !params["approvalPolicy"].is_null() && params["approvalPolicy"] != "never" {
                    context.approval_policy = Some(Json(params["approvalPolicy"].clone()));
                }
                if params["sandboxPolicy"].is_object()
                    && params["sandboxPolicy"]["type"] != "dangerFullAccess"
                {
                    context.sandbox_policy = Some(Json(params["sandboxPolicy"].clone()));
                }
                if params["collaborationMode"]["settings"]
                    .get("developer_instructions")
                    .is_some()
                {
                    context.developer_instructions = Some("<recorded>".into());
                }
            }
        }
        let root = ThreadId::new("root").unwrap();
        rig.create(
            &root,
            ModelSelection {
                instance: format!("{driver:?}"),
                driver,
                model: optional(&rows[0]["metadata"], "model")
                    .or_else(|| {
                        rows.iter()
                            .find(|row| row["frame"]["method"] == "turn/start")
                            .and_then(|row| optional(&row["frame"]["params"], "model"))
                    })
                    .unwrap_or_default(),
                options: BTreeMap::new(),
            },
            RuntimeMode::FullAccess,
        )
        .await;
        if rows.iter().any(|row| {
            row["type"] == "expect_outbound"
                && row["frame"]["method"] == "turn/start"
                && row["frame"]["params"]["collaborationMode"]["mode"] == "plan"
        }) {
            rig.command(
                &root,
                Command::InteractionMode {
                    mode: InteractionMode::Plan,
                },
            )
            .await;
        }
        let daemon = rig.worker.clone().spawn(DaemonOptions {
            concurrency: 4,
            liveness: Duration::from_millis(20),
        });
        Self {
            ignored_config: rows[0]["metadata"]["recorderThreadConfigKeys"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            rig,
            _daemon: daemon,
            driver,
            scenario: scenario.into(),
            root,
            rows,
            opens,
            errors,
            sessions,
            graph: Mutex::new(Graph::default()),
            injected,
        }
    }

    fn context(&self, index: usize) -> String {
        format!("{} {:?} row {index}", self.scenario, self.driver)
    }

    /// Waits until every frame was handled and every effect either finished or
    /// waits for the provider.
    async fn settle(&self, index: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut quiet = 0;
        loop {
            let errors = self.errors.lock().unwrap().clone();
            assert!(errors.is_empty(), "{}: {errors:?}", self.context(index));
            let drained = self
                .rig
                .host
                .processes
                .lock()
                .unwrap()
                .iter()
                .all(|process| process.stdout.drained());
            let (pending, running): (i64, i64) = self
                .rig
                .store
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT COALESCE(SUM(status = 'pending'), 0), COALESCE(SUM(status = 'running'), 0) FROM outbox",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?)
                })
                .unwrap();
            let awaiting = self.rig.sessions.awaiting_provider() as i64;
            if drained && pending == 0 && running == awaiting {
                quiet += 1;
                if quiet == 3 {
                    return;
                }
            } else {
                quiet = 0;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}: not settled (drained {drained}, pending {pending}, running {running}, awaiting {awaiting})",
                self.context(index)
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    async fn run(scenario: &str, driver: Driver) -> Outcome {
        let mut replay = Self::start(scenario, driver).await;
        let rows = std::mem::take(&mut replay.rows);
        for index in 0..rows.len() {
            replay.step(&rows, index).await;
        }
        replay.finish(&rows).await
    }

    async fn step(&self, rows: &[Value], index: usize) {
        let row = &rows[index];
        let frame = &row["frame"];
        match row["type"].as_str() {
            Some("emit_inbound") if frame["type"] == "session.forked" => self.forked(frame),
            Some("emit_inbound") => {
                let frame = if frame["type"] == "permission.request" {
                    json!({"type":"control_request","request_id":frame["options"]["toolUseID"],"request":{"subtype":"can_use_tool","tool_name":frame["toolName"],"input":frame["input"],"tool_use_id":frame["options"]["toolUseID"],"permission_suggestions":frame["options"]["suggestions"],"description":frame["options"]["description"]}})
                } else {
                    frame.clone()
                };
                let frame = self.mapped(self.recorded_reply(frame));
                self.live(index).emit(frame);
                self.settle(index).await;
            }
            Some("runtime_exit") => {
                self.live(index).exit(row["status"] == "success");
                self.rig
                    .until("the session closed", async || {
                        self.rig.sessions.sessions().is_empty()
                    })
                    .await;
                self.settle(index).await;
            }
            Some("expect_outbound") => match string(frame, "type").as_str() {
                "query.open" => {
                    self.opens.lock().unwrap().push_back(frame.clone());
                    self.opened(rows, index).await;
                }
                "session.fork" => self.fork_claude(frame, index).await,
                "subagent.lookup" => {}
                _ => self.expect(rows, index).await,
            },
            _ => {}
        }
    }

    async fn finish(self, rows: &[Value]) -> Outcome {
        self.settle(rows.len()).await;
        let unexpected: Vec<_> = self.rig.host.outbound.lock().unwrap().drain(..).collect();
        assert!(
            unexpected.is_empty(),
            "{} {:?}: unexpected outbound {unexpected:?}",
            self.scenario,
            self.driver
        );
        assert!(
            self.opens.lock().unwrap().is_empty(),
            "{} {:?}: a recorded process never opened",
            self.scenario,
            self.driver
        );
        Outcome::collect(self).await
    }

    fn live(&self, index: usize) -> Arc<super::fake::FakeProcess> {
        self.rig
            .host
            .last()
            .filter(|process| !process.exited())
            .unwrap_or_else(|| panic!("{}: no live provider process", self.context(index)))
    }

    async fn expect(&self, rows: &[Value], index: usize) {
        self.settle(index).await;
        if self.rig.host.outbound.lock().unwrap().is_empty() {
            self.user_action(rows, index).await;
            self.settle(index).await;
        }
        let expected = &rows[index]["frame"];
        let mut actual = self
            .rig
            .host
            .outbound
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                panic!(
                    "{}: expected {expected} but nothing was sent",
                    self.context(index)
                )
            });
        // The reference recorder offered some prompts without a message UUID.
        if expected["type"] == "prompt.offer" && expected["message"].get("uuid").is_none() {
            actual["message"]
                .as_object_mut()
                .map(|message| message.remove("uuid"));
        }
        let expected = &self.injected_turn(expected);
        if let Some(id) = actual["id"].as_u64() {
            actual["id"] = json!(id - self.skipped(id));
        }
        assert_eq!(
            normalized_frame(&actual, &self.ignored_config),
            normalized_frame(expected, &self.ignored_config),
            "{}",
            self.context(index)
        );
    }

    async fn state(&self) -> Arc<State> {
        self.rig.state(&self.root).await
    }

    async fn send(&self, text: String, steer: bool) {
        self.send_to(&self.root.clone(), text, steer).await;
    }

    async fn send_to(&self, thread: &ThreadId, text: String, steer: bool) {
        let mode = if steer {
            DispatchMode::SteerActive {
                run: self
                    .rig
                    .state(thread)
                    .await
                    .active_run()
                    .unwrap()
                    .id
                    .clone(),
            }
        } else {
            DispatchMode::QueueAfterActive
        };
        let reply = self.rig.send(thread, &text, mode).await;
        assert!(!matches!(reply, Reply::Rejected { .. }), "{reply:?}");
    }

    async fn respond(&self, frame: &Value) {
        let Some(request) = self
            .state()
            .await
            .requests
            .iter()
            .find(|r| r.status == RequestStatus::Pending)
            .cloned()
        else {
            return;
        };
        let result = &frame["result"];
        let decision = match result["behavior"].as_str().or(result["decision"].as_str()) {
            Some("deny" | "decline") => ApprovalDecision::Decline,
            Some("cancel") => ApprovalDecision::Cancel,
            Some("acceptForSession") => ApprovalDecision::AcceptForSession,
            _ => ApprovalDecision::Accept,
        };
        let answers = result["answers"].as_object().map(|a| {
            a.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        Answer::Choices(
                            value["answers"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|v| v.as_str().unwrap().into())
                                .collect(),
                        ),
                    )
                })
                .collect()
        });
        self.rig
            .command(
                &self.root,
                Command::Respond {
                    request: request.id,
                    decision: if answers.is_some() {
                        None
                    } else {
                        Some(decision)
                    },
                    answers,
                    attachments: BTreeMap::new(),
                },
            )
            .await;
    }

    /// Loaded threads and their states.
    async fn threads(&self) -> Vec<(ThreadId, Arc<State>)> {
        let mut threads = self.rig.registry.loaded();
        threads.sort();
        let mut states = vec![];
        for thread in threads {
            let state = self.rig.state(&thread).await;
            states.push((thread, state));
        }
        states
    }

    /// The app thread bound to a native thread or session.
    async fn thread_of(&self, native: &str) -> ThreadId {
        self.threads()
            .await
            .into_iter()
            .find(|(_, state)| state.native_sessions.values().any(|bound| bound == native))
            .map(|(thread, _)| thread)
            .unwrap_or_else(|| self.root.clone())
    }

    /// The latest completed run whose turn ended at `head`.
    fn run_at(state: &State, head: Option<&str>) -> RunId {
        state
            .runs
            .iter()
            .rev()
            .find(|run| {
                run.status.terminal()
                    && run.status != RunStatus::RolledBack
                    && head.is_none_or(|head| {
                        run.attempt
                            .as_ref()
                            .and_then(|id| state.attempts.iter().find(|a| &a.id == id))
                            .is_some_and(|a| a.native_head.as_deref() == Some(head))
                    })
            })
            .unwrap()
            .id
            .clone()
    }

    /// The user forks `parent` through the run that ended at `head`. As in T3,
    /// the provider forks when the child sends its first message.
    async fn fork(&self, parent: &ThreadId, head: Option<&str>) -> ThreadId {
        let through_run = Self::run_at(&*self.rig.state(parent).await, head);
        let target = {
            let graph = self.graph.lock().unwrap();
            ThreadId::new(format!("fork-{}", graph.forks.len())).unwrap()
        };
        let reply = self
            .rig
            .command(
                parent,
                Command::Fork {
                    target: target.clone(),
                    source: SourcePoint::Run(through_run),
                    title: Some("Forked thread".into()),
                    created_by: agent_domain::MessageAuthor::User,
                    creation_source: "desktop".into(),
                },
            )
            .await;
        assert_eq!(reply, Reply::Thread(target.clone()));
        self.graph.lock().unwrap().forks.push(target.clone());
        target
    }

    /// The user rolls `thread` back to the checkpoint whose native head is `head`.
    async fn rollback(&self, thread: &ThreadId, head: Option<&str>) {
        let state = self.rig.state(thread).await;
        let instance = state.thread.as_ref().unwrap().selection.instance.clone();
        let checkpoint = state
            .checkpoints
            .iter()
            .find(|cp| cp.native_heads.get(&instance).and_then(Option::as_deref) == head)
            .unwrap_or_else(|| panic!("{}: no checkpoint at {head:?}", self.scenario))
            .id
            .clone();
        assert_eq!(
            self.rig
                .command(
                    thread,
                    Command::Rollback {
                        checkpoint,
                        restore_files: false,
                        restore_refusal: None,
                    },
                )
                .await,
            Reply::Accepted
        );
    }

    /// A recorded Claude query opens for the thread holding its session; a
    /// resume at a message is the user's rollback to it. Opening a recorded
    /// fork session is the fork child's first message.
    async fn opened(&self, rows: &[Value], index: usize) {
        let options = &rows[index]["frame"]["options"];
        let unconsumed = self.graph.lock().unwrap().unconsumed.take();
        if let Some((recorded, child)) = unconsumed {
            if options["resume"].as_str() == Some(recorded.as_str()) {
                self.consume_claude_fork(rows, index, recorded, child).await;
                return;
            }
            self.graph.lock().unwrap().unconsumed = Some((recorded, child));
        }
        let thread = match options["resume"].as_str() {
            Some(recorded) => {
                let session = self
                    .sessions
                    .lock()
                    .unwrap()
                    .get(recorded)
                    .cloned()
                    .unwrap_or_else(|| recorded.to_owned());
                self.thread_of(&session).await
            }
            None => self.root.clone(),
        };
        self.graph.lock().unwrap().current = Some(thread.clone());
        if let Some(head) = options["resumeSessionAt"].as_str() {
            self.settle(index).await;
            self.rollback(&thread, Some(head)).await;
            self.settle(index).await;
            assert!(
                self.rig.state(&thread).await.rollback.is_none(),
                "{}",
                self.context(index)
            );
        }
    }

    async fn fork_claude(&self, frame: &Value, index: usize) {
        self.settle(index).await;
        let source = self
            .sessions
            .lock()
            .unwrap()
            .get(&string(frame, "sessionId"))
            .cloned()
            .unwrap_or_else(|| string(frame, "sessionId"));
        let parent = self.thread_of(&source).await;
        let target = self
            .fork(&parent, frame["options"]["upToMessageId"].as_str())
            .await;
        self.settle(index).await;
        self.graph.lock().unwrap().forking = Some(target);
    }

    /// Injection requests sent before the request `id`, absent from the recording.
    fn skipped(&self, id: u64) -> u64 {
        self.injected
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["id"].as_u64().is_some_and(|injected| injected < id))
            .count() as u64
    }

    /// A recorded reply answers the request the runtime numbered after its
    /// injections.
    fn recorded_reply(&self, mut frame: Value) -> Value {
        if frame.get("method").is_none()
            && let Some(recorded) = frame["id"].as_u64()
        {
            let injected: Vec<u64> = self
                .injected
                .lock()
                .unwrap()
                .iter()
                .filter_map(|frame| frame["id"].as_u64())
                .collect();
            let mut id = recorded;
            while injected.contains(&id) || id - self.skipped(id) < recorded {
                id += 1;
            }
            frame["id"] = json!(id);
        }
        frame
    }

    /// With the history injected, a recorded inline handoff is the user's text.
    fn injected_turn(&self, expected: &Value) -> Value {
        let mut expected = expected.clone();
        if expected["method"] == "turn/start"
            && !self.injected.lock().unwrap().is_empty()
            && let Some(input) = expected["params"]["input"].as_array_mut()
        {
            for part in input {
                if let Some((header, user)) = part["text"]
                    .as_str()
                    .and_then(|text| text.split_once("\n\nUser message:\n"))
                    && header.starts_with("Context handoff (")
                {
                    part["text"] = json!(user);
                }
            }
        }
        expected
    }

    /// A recorded frame naming the sessions the runtime created instead.
    fn mapped(&self, frame: Value) -> Value {
        fn walk(value: Value, sessions: &BTreeMap<String, String>) -> Value {
            match value {
                Value::String(text) => Value::String(sessions.get(&text).cloned().unwrap_or(text)),
                Value::Array(values) => {
                    Value::Array(values.into_iter().map(|v| walk(v, sessions)).collect())
                }
                Value::Object(map) => Value::Object(
                    map.into_iter()
                        .map(|(key, value)| (key, walk(value, sessions)))
                        .collect(),
                ),
                other => other,
            }
        }
        let sessions = self.sessions.lock().unwrap().clone();
        if sessions.is_empty() {
            frame
        } else {
            walk(frame, &sessions)
        }
    }

    /// The recorded fork session is the one the runtime writes for the child
    /// once it sends its first message.
    fn forked(&self, frame: &Value) {
        let mut graph = self.graph.lock().unwrap();
        let target = graph.forking.take().unwrap();
        graph.unconsumed = Some((string(frame, "sessionId"), target));
    }

    /// The child's first message forks the Claude session, which the recorded
    /// fork session then names.
    async fn consume_claude_fork(
        &self,
        rows: &[Value],
        index: usize,
        recorded: String,
        child: ThreadId,
    ) {
        let sessions = self.sessions.clone();
        let pending = Mutex::new(Some(recorded));
        *self.rig.host.before_session_write.lock().unwrap() =
            Some(Arc::new(move |session: &str| {
                if let Some(recorded) = pending.lock().unwrap().take() {
                    sessions
                        .lock()
                        .unwrap()
                        .insert(recorded, session.to_owned());
                }
            }));
        self.graph.lock().unwrap().current = Some(child.clone());
        let prompt = rows[index..]
            .iter()
            .find(|row| row["type"] == "expect_outbound" && row["frame"]["type"] == "prompt.offer")
            .map(|row| Self::prompt(&row["frame"]))
            .unwrap();
        self.user_turn(&child, prompt, false).await;
        self.settle(index).await;
        *self.rig.host.before_session_write.lock().unwrap() = None;
    }

    /// The prompt text of a recorded turn.
    fn prompt(frame: &Value) -> String {
        if frame["type"] == "prompt.offer" {
            string(&frame["message"]["message"], "content")
        } else {
            frame["params"]["input"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// The user's message for a recorded turn of `thread`; a recorded merge-back
    /// handoff is the next fork merging back first.
    async fn user_turn(&self, thread: &ThreadId, text: String, steer: bool) {
        let text = match text.split_once("User message:\n") {
            Some((_, user)) if text.starts_with("Context handoff (") => {
                let source = {
                    let mut graph = self.graph.lock().unwrap();
                    graph.merges += 1;
                    graph.forks[graph.merges - 1].clone()
                };
                assert_eq!(
                    self.rig
                        .command(
                            &source,
                            Command::MergeBack {
                                target: thread.clone(),
                                source: SourcePoint::LatestStable,
                            },
                        )
                        .await,
                    Reply::Accepted
                );
                // The transfer reaches the target before the user's message.
                self.settle(0).await;
                user.to_owned()
            }
            _ => text,
        };
        self.send_to(thread, text, steer).await;
    }

    /// The user action an expected outbound frame implies when the runtime has
    /// nothing left to send.
    async fn user_action(&self, rows: &[Value], index: usize) {
        let frame = &rows[index]["frame"];
        let method = string(frame, "method");
        let kind = string(frame, "type");
        let native = |frame: &Value| string(&frame["params"], "threadId");
        match (method.as_str(), kind.as_str()) {
            ("thread/start", _)
                if self.scenario == "delegated_task_status"
                    && !self.state().await.native_sessions.is_empty() =>
            {
                self.delegate(rows, index).await;
            }
            (
                "initialize"
                | "thread/start"
                | "thread/resume"
                | "thread/inject_items"
                | "turn/start"
                | "thread/fork"
                | "thread/read",
                _,
            ) => {
                // The first user-visible operation the frames lead to.
                let (at, next) = rows[index..]
                    .iter()
                    .enumerate()
                    .find(|(_, row)| {
                        row["type"] == "expect_outbound"
                            && matches!(
                                row["frame"]["method"].as_str(),
                                Some("turn/start" | "thread/fork" | "thread/read")
                            )
                    })
                    .map(|(at, row)| (index + at, &row["frame"]))
                    .unwrap();
                match next["method"].as_str() {
                    Some("thread/fork") => {
                        let parent = self.thread_of(&native(next)).await;
                        let child = self
                            .fork(&parent, next["params"]["lastTurnId"].as_str())
                            .await;
                        self.settle(index).await;
                        let first = rows[at + 1..]
                            .iter()
                            .find(|row| {
                                row["type"] == "expect_outbound"
                                    && row["frame"]["method"] == "turn/start"
                            })
                            .map(|row| Self::prompt(&row["frame"]))
                            .unwrap();
                        self.user_turn(&child, first, false).await;
                    }
                    Some("thread/read") => self.codex_rollback(rows, at).await,
                    _ => {
                        let thread = self.thread_of(&native(next)).await;
                        self.user_turn(&thread, Self::prompt(next), false).await;
                        if self.scenario == "queued_turn" && self.state().await.runs.len() == 1 {
                            let second = rows[at + 1..]
                                .iter()
                                .find(|row| {
                                    row["type"] == "expect_outbound"
                                        && row["frame"]["method"] == "turn/start"
                                })
                                .map(|row| Self::prompt(&row["frame"]))
                                .unwrap();
                            self.send(second, false).await;
                        }
                    }
                }
            }
            ("turn/steer", _) => self.send(Self::prompt(frame), true).await,
            (_, "prompt.offer") => {
                let steer = frame["message"]["priority"] == "now";
                let thread = self
                    .graph
                    .lock()
                    .unwrap()
                    .current
                    .clone()
                    .unwrap_or_else(|| self.root.clone());
                self.user_turn(&thread, Self::prompt(frame), steer).await;
                if self.scenario == "queued_turn" && self.state().await.runs.len() == 1 {
                    let second = rows[index + 1..]
                        .iter()
                        .find(|row| {
                            row["type"] == "expect_outbound"
                                && row["frame"]["type"] == "prompt.offer"
                        })
                        .map(|row| Self::prompt(&row["frame"]))
                        .unwrap();
                    self.send(second, false).await;
                }
            }
            ("turn/interrupt", _) if self.graph.lock().unwrap().delegated.is_some() => {
                let child = self.thread_of(&native(frame)).await;
                let run = self
                    .rig
                    .state(&child)
                    .await
                    .active_run()
                    .unwrap()
                    .id
                    .clone();
                assert_eq!(
                    self.rig
                        .command(
                            &child,
                            Command::Interrupt {
                                run,
                                hold_queue: false,
                                reason: None,
                            },
                        )
                        .await,
                    Reply::Accepted
                );
            }
            ("turn/interrupt" | "thread/backgroundTerminals/terminate", _) => {
                let thread = self.thread_of(&native(frame)).await;
                self.rig.command(&thread, Command::Stop).await;
            }
            (_, "query.interrupt") => {
                self.rig.command(&self.root, Command::Stop).await;
            }
            ("thread/compact/start", _) => {
                self.rig.command(&self.root, Command::Compact).await;
            }
            ("", "permission.response") | ("", "") if frame.get("result").is_some() => {
                self.respond(frame).await
            }
            _ => {}
        }
    }

    /// The recorded revert names the oldest discarded turn; the user rolls back
    /// to the checkpoint of the run before it.
    async fn codex_rollback(&self, rows: &[Value], index: usize) {
        let thread = self
            .thread_of(&string(&rows[index]["frame"]["params"], "threadId"))
            .await;
        let discarded = rows[index..]
            .iter()
            .find(|row| {
                row["type"] == "expect_outbound" && row["frame"]["method"] == "thread/revert"
            })
            .and_then(|row| row["frame"]["params"]["beforeTurnId"].as_str())
            .unwrap();
        let state = self.rig.state(&thread).await;
        let run = state
            .runs
            .iter()
            .find(|run| {
                run.attempt
                    .as_ref()
                    .and_then(|id| state.attempts.iter().find(|a| &a.id == id))
                    .is_some_and(|a| a.native_head.as_deref() == Some(discarded))
            })
            .unwrap();
        let instance = state.thread.as_ref().unwrap().selection.instance.clone();
        let head = state
            .checkpoints
            .iter()
            .find(|cp| cp.run_ordinal == run.ordinal - 1)
            .and_then(|cp| cp.native_heads.get(&instance).cloned().flatten());
        self.rollback(&thread, head.as_deref()).await;
    }

    /// The parent delegates the recorded child task.
    async fn delegate(&self, rows: &[Value], index: usize) {
        let prompt = rows[index..]
            .iter()
            .find(|row| row["type"] == "expect_outbound" && row["frame"]["method"] == "turn/start")
            .map(|row| Self::prompt(&row["frame"]))
            .unwrap();
        let child = ThreadId::new("delegated-child").unwrap();
        let parent = self.state().await.thread.clone().unwrap();
        assert_eq!(
            self.rig
                .command(
                    &self.root,
                    Command::Delegate {
                        task: NodeId::new("delegated-task").unwrap(),
                        child: child.clone(),
                        prompt,
                        title: None,
                        selection: parent.selection.clone(),
                        runtime_mode: parent.runtime_mode,
                        interaction_mode: parent.interaction_mode,
                        wake: CompletionWake::SettledOnly,
                    },
                )
                .await,
            Reply::Thread(child.clone())
        );
        self.graph.lock().unwrap().delegated = Some(child);
    }
}

/// The committed projections after a replay.
struct Outcome {
    replay: Replay,
    root: ThreadId,
    states: BTreeMap<ThreadId, State>,
    facts: Vec<Fact>,
}
impl Outcome {
    async fn collect(replay: Replay) -> Self {
        let root = replay.root.clone();
        let mut outcome = Self {
            root: root.clone(),
            states: BTreeMap::new(),
            facts: vec![],
            replay,
        };
        outcome.refresh().await;
        let store = outcome.replay.rig.store.clone();
        outcome.facts = store
            .blocking(move |store| store.facts_after(Some(&root), 0))
            .await
            .unwrap()
            .into_iter()
            .map(|stored| stored.fact)
            .collect();
        outcome
    }
    async fn refresh(&mut self) {
        let mut threads = self.replay.rig.registry.loaded();
        threads.sort();
        self.states.clear();
        for thread in threads {
            let state = self.replay.rig.state(&thread).await;
            self.states.insert(thread, (*state).clone());
        }
    }
    async fn command(&mut self, thread: &ThreadId, command: Command) -> Reply {
        let reply = self.replay.rig.command(thread, command).await;
        self.refresh().await;
        reply
    }
    fn state(&self) -> &State {
        &self.states[&self.root]
    }
    fn statuses(&self) -> Vec<RunStatus> {
        self.state().runs.iter().map(|r| r.status).collect()
    }
    fn replies(&self, run: &RunId) -> Vec<String> {
        self.state()
            .items
            .iter()
            .filter(|i| {
                i.run.as_ref() == Some(run) && matches!(i.kind, ItemKind::AssistantMessage { .. })
            })
            .map(|i| i.text.trim().to_owned())
            .collect()
    }
    fn integrity(&self) {
        for state in self.states.values() {
            assert!(state.runs.iter().filter(|r| r.status.blocking()).count() <= 1);
            for run in &state.runs {
                assert!(state.messages.iter().any(|m| m.id == run.message));
            }
            for item in &state.items {
                assert!(
                    item.run
                        .as_ref()
                        .is_none_or(|run| state.runs.iter().any(|r| &r.id == run))
                );
                if let ItemKind::AssistantMessage { message } | ItemKind::UserMessage { message } =
                    &item.kind
                {
                    assert!(state.messages.iter().any(|m| &m.id == message));
                }
            }
        }
    }
}

async fn run(scenario: &str, driver: Driver) -> Outcome {
    Replay::run(scenario, driver).await
}

/// Every reference transcript has a port below.
#[test]
fn every_transcript_is_replayed() {
    let manifest: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../agent-providers/src/fixtures/manifest.json"
    ))
    .unwrap();
    let mut expected: Vec<String> = manifest.iter().map(|entry| string(entry, "file")).collect();
    expected.sort();
    let source = include_str!("replay.rs");
    let mut covered: Vec<String> = expected
        .iter()
        .filter(|file| {
            let (scenario, name) = file.split_once('/').unwrap();
            let driver = if name.starts_with("codex") {
                "Codex"
            } else {
                "Claude"
            };
            let quoted = format!("\"{scenario}\"");
            source.lines().any(|line| {
                line.contains(&quoted)
                    && (line.contains(&format!("Driver::{driver}")) || !line.contains("Driver::"))
            })
        })
        .cloned()
        .collect();
    covered.sort();
    assert_eq!(covered, expected);
    assert_eq!(expected.len(), 71);
    assert_eq!(
        expected.iter().filter(|file| graph(file)).count(),
        18,
        "{GRAPH:?}"
    );
}

// Ports of the agent-providers replay tests, expectations unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn queued_turn_replays_preserve_queued_acceptance_and_promotion_order() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = run("queued_turn", driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed; 2]);
        assert_eq!(
            replay
                .state()
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .map(|m| m.intent)
                .collect::<Vec<_>>(),
            vec![InputIntent::TurnStart, InputIntent::QueuedTurn]
        );
        assert_eq!(
            replay.replies(&replay.state().runs[0].id),
            vec!["first fixture turn complete"]
        );
        assert_eq!(
            replay.replies(&replay.state().runs[1].id),
            vec!["second fixture turn complete"]
        );
        assert!(replay.facts.iter().any(|f|matches!(&f.body,FactBody::RunRequested { id,status:RunStatus::Queued,.. } if *id == replay.state().runs[1].id)));
        assert_eq!(
            replay
                .state()
                .items
                .iter()
                .map(|i| i.run.clone().unwrap())
                .collect::<Vec<_>>(),
            vec![
                replay.state().runs[0].id.clone(),
                replay.state().runs[0].id.clone(),
                replay.state().runs[1].id.clone(),
                replay.state().runs[1].id.clone()
            ]
        );
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn steering_replays_keep_one_run_and_one_attempt() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = run("message_steering", driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        assert_eq!(replay.state().attempts.len(), 1);
        assert_eq!(
            replay
                .state()
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .map(|m| m.intent)
                .collect::<Vec<_>>(),
            vec![InputIntent::TurnStart, InputIntent::Steer]
        );
        assert!(
            replay
                .replies(&replay.state().runs[0].id)
                .iter()
                .any(|text| text.contains("steering fixture observed"))
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn simple_replays_have_the_original_roles_items_ordinals_and_reply() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = run("simple", driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        assert_eq!(replay.state().runs[0].ordinal, 1);
        assert_eq!(
            replay
                .state()
                .messages
                .iter()
                .map(|m| m.role)
                .collect::<Vec<_>>(),
            vec![Role::User, Role::Assistant]
        );
        assert_eq!(replay.state().items.len(), 2);
        assert_eq!(
            replay.replies(&replay.state().runs[0].id),
            vec!["fixture simple ok"]
        );
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn multi_turn_replays_share_the_native_thread_and_preserve_replies() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = run("multi_turn", driver).await;
        replay.integrity();
        assert_eq!(
            replay.statuses(),
            vec![RunStatus::Completed, RunStatus::Completed]
        );
        assert_eq!(
            replay.replies(&replay.state().runs[0].id),
            vec!["first fixture turn complete"]
        );
        assert_eq!(
            replay.replies(&replay.state().runs[1].id),
            vec!["second fixture turn complete"]
        );
        assert_eq!(
            replay.state().attempts[0].native_thread,
            replay.state().attempts[1].native_thread
        );
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn compact_after_peer_turn_obeys_the_reference_echo_and_no_echo_paths() {
    let replay = run("claude_compact_after_peer_turn", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed; 3]);
    assert_eq!(
        replay.replies(&replay.state().runs[0].id),
        vec!["compact probe first turn"]
    );
    assert_eq!(
        replay.replies(&replay.state().runs[1].id),
        Vec::<String>::new()
    );
    assert_eq!(replay.replies(&replay.state().runs[2].id), vec!["PEER_ACK"]);
    assert!(
        replay
            .state()
            .messages
            .iter()
            .find(|m| m.id == replay.state().runs[2].message)
            .unwrap()
            .created_by
            == MessageAuthor::Agent
    );
    assert_eq!(
        replay
            .state()
            .items
            .iter()
            .filter_map(|i| if let ItemKind::Compaction { before, after } = i.kind {
                Some((i.run.clone(), before, after))
            } else {
                None
            })
            .collect::<Vec<_>>(),
        vec![(
            Some(replay.state().runs[1].id.clone()),
            Some(27445),
            Some(1192)
        )]
    );
    let replay = run("claude_compact_after_peer_turn_no_echo", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed; 2]);
    assert_eq!(replay.replies(&replay.state().runs[1].id), vec!["PEER_ACK"]);
    assert!(
        !replay
            .state()
            .items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::Compaction { .. }))
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn proposed_plan_and_todo_replays_keep_the_original_plan_expectations() {
    let replay = run("proposed_plan", Driver::Codex).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
    let plan = replay.state().plans.last().unwrap();
    assert!(plan.markdown.to_lowercase().contains("replay"));
    assert!(plan.markdown.to_lowercase().contains("fixture"));
    assert!(plan.implemented_by.is_none());
    assert!(
        replay
            .state()
            .items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::ProposedPlan { .. })
                && i.status == ItemStatus::Completed)
    );
    {
        let driver = Driver::Codex;
        let replay = run("todo_list", driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        assert_eq!(
            replay
                .state()
                .plans
                .last()
                .unwrap()
                .steps
                .iter()
                .map(|s| s.status.as_str())
                .collect::<Vec<_>>(),
            vec!["completed"; 3]
        );
        assert!(
            replay
                .state()
                .items
                .iter()
                .any(|i| matches!(i.kind, ItemKind::CommandExecution { .. }))
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_command_and_monitor_replays_keep_roster_notifications_and_wake_ownership() {
    for (scenario, kind, description, count) in [
        (
            "claude_background_task_wake",
            BackgroundKind::Command,
            "Background sleep test",
            3,
        ),
        (
            "claude_background_monitor_wake",
            BackgroundKind::Monitor,
            "Background monitor test",
            2,
        ),
    ] {
        let replay = run(scenario, Driver::Claude).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed; count]);
        assert_eq!(replay.replies(&replay.state().runs[0].id), vec!["STARTED"]);
        assert_eq!(
            replay.replies(&replay.state().runs[1].id),
            vec!["WAKE_DONE"]
        );
        assert!(replay.state().background_work.is_empty());
        assert!(replay.state().tasks.is_empty());
        assert!(replay.facts.iter().any(|f|matches!(&f.body,FactBody::BackgroundTaskStarted { kind:k,description:d,.. } if *k == kind && d == description)));
        let notifications = replay
            .state()
            .activity_items()
            .into_iter()
            .filter_map(|i| {
                if let ItemKind::Notification {
                    notification:
                        Notification {
                            summary,
                            outcome,
                            source,
                            ..
                        },
                } = &i.kind
                {
                    Some((summary.clone(), *outcome, source.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let label = if kind == BackgroundKind::Command {
            "Command"
        } else {
            "Monitor"
        };
        assert_eq!(
            notifications,
            vec![(
                format!("{label} \"{description}\" finished"),
                NotificationOutcome::Completed,
                NotificationSource::Native(kind)
            )]
        );
        if count == 3 {
            assert_eq!(
                replay.replies(&replay.state().runs[2].id),
                vec!["USER_REPLY"]
            );
            assert_eq!(
                replay
                    .state()
                    .messages
                    .iter()
                    .find(|m| m.id == replay.state().runs[1].message)
                    .unwrap()
                    .text,
                "Background command \"Background sleep test\" completed (exit code 0)"
            );
        }
    }
}
// T3 fixtures/tool_call_read_only_on_request/output.ts.
#[tokio::test(flavor = "multi_thread")]
async fn read_only_on_request_replays_ask_once_and_run_the_approved_write() {
    const PROBE_FILE: &str = ".codex-probe-write-action.txt";
    const PROBE_CONTENT: &str = "codex app-server approval fixture";
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = run("tool_call_read_only_on_request", driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        let state = replay.state();
        assert_eq!(state.requests.len(), 1, "{driver:?}");
        let request = &state.requests[0];
        assert_eq!(request.status, RequestStatus::Resolved);
        assert_eq!(request.decision, Some(ApprovalDecision::Accept));
        let kind = request_kind(request);
        assert!(["command", "file-change"].contains(&kind), "{kind}");
        let approvals = state
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::ApprovalRequest { request } => Some(request),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(approvals, [&request.id]);
        let writes = state
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::CommandExecution { command, .. }
                    if kind == "command" && command.contains(PROBE_FILE) =>
                {
                    Some((item.status, command.clone()))
                }
                ItemKind::FileChange { changes }
                    if kind == "file-change" && changes.0.to_string().contains(PROBE_FILE) =>
                {
                    Some((item.status, changes.0.to_string()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!writes.is_empty(), "{driver:?}");
        assert!(
            writes
                .iter()
                .any(|(status, _)| *status == ItemStatus::Completed)
        );
        assert!(
            writes
                .iter()
                .all(|(_, content)| content.contains(PROBE_CONTENT))
        );
    }
}
// T3 fixtures/tool_call_restricted_granular/output.ts and claude_output.ts.
#[tokio::test(flavor = "multi_thread")]
async fn restricted_granular_replays_resolve_one_request_of_the_reference_kind() {
    for (driver, kind) in [(Driver::Codex, "file-change"), (Driver::Claude, "command")] {
        let replay = run("tool_call_restricted_granular", driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        let state = replay.state();
        assert_eq!(
            state.requests.iter().map(request_kind).collect::<Vec<_>>(),
            [kind]
        );
        assert!(
            state
                .requests
                .iter()
                .all(|r| r.status == RequestStatus::Resolved)
        );
        assert!(has_item(state, |k| matches!(
            k,
            ItemKind::UserMessage { .. }
        )));
        assert!(has_item(state, |k| matches!(
            k,
            ItemKind::ApprovalRequest { .. }
        )));
        assert!(has_item(state, |k| matches!(
            k,
            ItemKind::AssistantMessage { .. }
        )));
        if driver == Driver::Claude {
            assert!(has_item(state, |k| matches!(
                k,
                ItemKind::CommandExecution { .. }
            )));
            assert!(
                replay
                    .replies(&state.runs[0].id)
                    .join("\n")
                    .contains("codex app-server approval fixture")
            );
        } else {
            assert!(has_item(state, |k| matches!(
                k,
                ItemKind::FileChange { .. }
            )));
        }
    }
}
// T3 fixtures/tool_call_denied_write/claude_output.ts.
#[tokio::test(flavor = "multi_thread")]
async fn denied_write_replay_declines_once_and_fails_the_write() {
    let replay = run("tool_call_denied_write", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
    let state = replay.state();
    assert_eq!(
        state.requests.iter().map(request_kind).collect::<Vec<_>>(),
        ["file-change"]
    );
    assert_eq!(state.requests[0].status, RequestStatus::Resolved);
    assert_eq!(state.requests[0].decision, Some(ApprovalDecision::Decline));
    let writes = state
        .items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::FileChange { .. }))
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].status, ItemStatus::Failed);
    assert!(
        replay
            .replies(&state.runs[0].id)
            .join("\n")
            .contains("write permission denied")
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn authentication_failure_replay_preserves_failure_details_and_allows_the_followup() {
    let replay = run("claude_result_is_error", Driver::Claude).await;
    replay.integrity();
    assert_eq!(
        replay.statuses(),
        vec![RunStatus::Failed, RunStatus::Completed]
    );
    assert_eq!(
        replay.replies(&replay.state().runs[0].id),
        vec!["Failed to authenticate. API Error: 401 Invalid authentication credentials"]
    );
    assert_eq!(
        replay.replies(&replay.state().runs[1].id),
        vec!["claude result is_error fixture recovered"]
    );
    let error = replay
        .state()
        .items
        .iter()
        .find(|i| matches!(i.kind, ItemKind::Error { .. }))
        .unwrap();
    let ItemKind::Error {
        message,
        code,
        class,
        ..
    } = &error.kind
    else {
        unreachable!()
    };
    assert_eq!(
        message,
        "Claude could not authenticate. For subscription login, run `claude auth login` on this environment's machine, then start a new thread. For API-key authentication, check this instance's configured credentials."
    );
    assert_eq!(code.as_deref(), Some("api_error_401"));
    assert_eq!(class.as_deref(), Some("provider_error"));
}
#[tokio::test(flavor = "multi_thread")]
async fn native_subagent_replays_keep_children_runless_and_output_out_of_the_parent() {
    for (scenario, driver, children) in [
        ("subagent", Driver::Claude, 2),
        ("subagent", Driver::Codex, 2),
        ("subagent_v2", Driver::Codex, 1),
        ("subagent_v2_nested", Driver::Codex, 3),
    ] {
        let replay = run(scenario, driver).await;
        replay.integrity();
        assert_eq!(
            replay.statuses(),
            vec![RunStatus::Completed],
            "{scenario} {driver:?}"
        );
        assert_eq!(replay.states.len(), children + 1, "{scenario} {driver:?}");
        for (id, child) in &replay.states {
            if id != &replay.root {
                assert!(child.runs.is_empty());
                assert!(child.thread.as_ref().unwrap().parent.is_some());
            }
        }
        assert!(
            !replay
                .state()
                .items
                .iter()
                .any(|i| matches!(i.kind, ItemKind::DynamicTool { .. }))
        );
        // subagent/codex_output.ts: child commands stay out of the parent.
        assert!(
            !replay
                .state()
                .items
                .iter()
                .any(|i| matches!(i.kind, ItemKind::CommandExecution { .. })),
            "{scenario} {driver:?}: a child command reached the parent"
        );
        if scenario == "subagent" {
            // Each child was handed one file and reported what only that file holds.
            let pairs = match driver {
                Driver::Codex => [
                    ("package.json", "tsconfig.json", "effect-codex-app-server"),
                    ("tsconfig.json", "package.json", "../../tsconfig.base.json"),
                ],
                Driver::Claude => [
                    ("package.json", "tsconfig.json", "claude-read-only-fixture"),
                    ("tsconfig.json", "package.json", "ES2022"),
                ],
            };
            let tasks = &replay.state().tasks;
            for (file, other, content) in pairs {
                assert!(
                    tasks.iter().any(|task| task.prompt.contains(file)
                        && !task.prompt.contains(other)
                        && task.result.as_deref().is_some_and(|r| r.contains(content))),
                    "{scenario} {driver:?}: no child read only {file}: {tasks:?}"
                );
            }
            if driver == Driver::Claude {
                for file in ["package.json", "tsconfig.json"] {
                    let prompt = format!(
                        "Read the file `{file}` in the current working directory and return its full contents."
                    );
                    assert!(tasks.iter().any(|task| task.prompt == prompt));
                }
            }
        }
        for state in replay.states.values() {
            for task in &state.tasks {
                assert_eq!(
                    task.status,
                    ItemStatus::Completed,
                    "{scenario} {driver:?}: {task:?}"
                );
                assert!(task.completed_at.is_some());
                if scenario.starts_with("subagent_v2") {
                    assert_eq!(task.prompt, "");
                    assert_eq!(task.result.as_deref(), Some("Hello."));
                    assert!(task.title.as_ref().unwrap().starts_with("/root/"));
                }
                let child = &replay.states[&task.child_thread];
                assert!(
                    child
                        .items
                        .iter()
                        .any(|i| matches!(i.kind, ItemKind::AssistantMessage { .. })),
                    "{scenario} {driver:?}: child has no assistant response"
                );
                let result = task.result.as_deref().unwrap_or_default();
                let head: String = result.chars().take(40).collect();
                assert!(
                    !head.is_empty()
                        && child.items.iter().any(|i| {
                            matches!(i.kind, ItemKind::AssistantMessage { .. })
                                && i.text.contains(&head)
                        }),
                    "{scenario} {driver:?}: child {} does not hold its own response {head:?}",
                    task.child_thread
                );
                if driver == Driver::Claude {
                    assert!(
                        child
                            .messages
                            .iter()
                            .any(|m| m.role == Role::User && m.text == task.prompt)
                    );
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_replays_close_attempts_tools_and_interrupt_rows() {
    for driver in [Driver::Claude, Driver::Codex] {
        for scenario in ["turn_interrupt", "turn_interrupt_mid_tool"] {
            let replay = run(scenario, driver).await;
            replay.integrity();
            assert_eq!(
                replay.statuses(),
                vec![RunStatus::Interrupted],
                "{driver:?} {scenario}"
            );
            assert_eq!(
                replay.state().attempts[0].status,
                AttemptStatus::Interrupted
            );
            let request = replay
                .state()
                .items
                .iter()
                .find(|i| matches!(i.kind, ItemKind::RunInterruptRequest))
                .unwrap();
            let result = replay.state().items.iter().find(|i| matches!(&i.kind, ItemKind::RunInterruptResult { request: id } if *id == request.id)).unwrap();
            assert_eq!(request.status, ItemStatus::Completed);
            assert_eq!(result.status, ItemStatus::Interrupted);
            if scenario == "turn_interrupt_mid_tool" {
                let tool = replay
                    .state()
                    .items
                    .iter()
                    .find(|i| matches!(i.kind, ItemKind::CommandExecution { .. }))
                    .unwrap();
                assert_eq!(tool.status, ItemStatus::Interrupted);
                assert!(tool.completed_at.is_some());
                assert!(
                    matches!(&tool.kind, ItemKind::CommandExecution { command, .. } if command.contains("node -e"))
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_child_approvals_are_owned_by_the_root_and_never_appear_in_child_threads() {
    for scenario in ["subagent_v2_approval", "subagent_v2_nested_approval"] {
        let replay = run(scenario, Driver::Codex).await;
        replay.integrity();
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        assert_eq!(replay.state().requests.len(), 1);
        let request = &replay.state().requests[0];
        assert_eq!(request.status, RequestStatus::Resolved);
        assert_eq!(request.decision, Some(ApprovalDecision::Accept));
        assert_eq!(
            Some(&request.attempt),
            replay.state().runs[0].attempt.as_ref()
        );
        assert!(!request.owner_path.is_empty());
        for child in replay.states.values().filter(|s| s.native_parent.is_some()) {
            assert!(child.requests.is_empty());
            assert!(
                !child
                    .items
                    .iter()
                    .any(|item| matches!(item.kind, ItemKind::ApprovalRequest { .. }))
            );
        }
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn local_bash_task_replay_keeps_command_output_without_child_threads() {
    let replay = run("claude_local_bash_task", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
    assert_eq!(replay.states.len(), 1);
    assert!(replay.state().tasks.is_empty());
    assert_eq!(
        replay.replies(&replay.state().runs[0].id),
        vec![
            "I'll run the typecheck command now.",
            "claude local bash task fixture complete"
        ]
    );
    let tool = replay
        .state()
        .items
        .iter()
        .find(|i| matches!(i.kind, ItemKind::CommandExecution { .. }))
        .unwrap();
    assert!(
        matches!(&tool.kind,ItemKind::CommandExecution { command,.. } if command.contains("vp run --filter @t3tools/web typecheck"))
    );
    assert!(tool.text.contains("tsgo --noEmit"));
}
#[tokio::test(flavor = "multi_thread")]
async fn web_search_replays_preserve_queries_and_structured_result_urls() {
    for driver in [Driver::Claude, Driver::Codex] {
        let replay = run("web_search", driver).await;
        assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
        assert!(replay.state().requests.is_empty());
        let searches: Vec<_> = replay
            .state()
            .items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::WebSearch { .. }))
            .collect();
        assert_eq!(searches.len(), 1);
        assert_eq!(searches[0].status, ItemStatus::Completed);
        let ItemKind::WebSearch { query, results } = &searches[0].kind else {
            panic!()
        };
        if driver == Driver::Claude {
            assert_eq!(query, "FIFA World Cup 2026 ticket pricing");
            assert!(results.as_ref().unwrap().0.to_string().contains("fifa.com"));
        } else {
            assert_eq!(query, "FIFA World Cup 2026 ticket prices official");
        }
        assert!(
            replay
                .replies(&replay.state().runs[0].id)
                .iter()
                .any(|text| text.contains("web search fixture complete"))
        );
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn mcp_tool_replay_keeps_recorded_presentation_and_leaves_absent_metadata_empty() {
    let replay = run("claude_mcp_tool_presentation", Driver::Claude).await;
    assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
    let tools: Vec<_> = replay
        .state()
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::DynamicTool {
                name, presentation, ..
            } => Some((i.status, name, presentation)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(),
        vec![
            "mcp__claude_ai_Firecrawl__firecrawl_scrape",
            "mcp__claude_ai_Firecrawl__firecrawl_map"
        ]
    );
    assert_eq!(tools[0].0, ItemStatus::Completed);
    assert_eq!(tools[0].2.title.as_deref(), Some("Firecrawl scrape"));
    assert_eq!(
        tools[0].2.source.as_ref().unwrap().0,
        json!({"key":"mcp:firecrawl","name":"Firecrawl","kind":"integration","icon":{"_tag":"themed-logo","logoUrl":"https://www.google.com/s2/favicons?domain=firecrawl.dev&sz=64"}})
    );
    assert_eq!(tools[1].0, ItemStatus::Completed);
    assert_eq!(*tools[1].2, ToolPresentation::default());
}
#[tokio::test(flavor = "multi_thread")]
async fn background_interrupt_replay_clears_the_roster_and_keeps_completed_launches() {
    let replay = run("claude_background_task_interrupt", Driver::Claude).await;
    assert_eq!(replay.statuses(), vec![RunStatus::Interrupted]);
    assert!(replay.state().background_work.is_empty());
    assert!(replay.state().tasks.is_empty());
    assert_eq!(
        replay
            .state()
            .items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::CommandExecution { .. }))
            .map(|i| i.status)
            .collect::<Vec<_>>(),
        vec![ItemStatus::Completed, ItemStatus::Interrupted]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn background_subagent_replay_keeps_child_work_and_only_the_child_completion_wakes_root() {
    for (scenario, expected_wake) in [
        ("claude_background_subagent_after_root", "SUB_FINAL_REPORT"),
        ("claude_nested_background_subagent_wake", "CHILD_DONE"),
    ] {
        let replay = run(scenario, Driver::Claude).await;
        replay.integrity();
        assert_eq!(
            replay.statuses(),
            vec![RunStatus::Completed; 2],
            "{scenario}"
        );
        assert!(replay.state().background_work.is_empty());
        let wake = &replay.state().runs[1];
        assert_eq!(
            replay
                .state()
                .messages
                .iter()
                .find(|m| m.id == wake.message)
                .unwrap()
                .text,
            expected_wake
        );
        let notifications: Vec<_> = replay
            .state()
            .activity_items()
            .into_iter()
            .filter(|i| matches!(i.kind, ItemKind::Notification { .. }))
            .collect();
        assert_eq!(notifications.len(), 1, "{scenario}");
        assert!(matches!(
            &notifications[0].kind,
            ItemKind::Notification {
                notification: Notification {
                    source: NotificationSource::Native(BackgroundKind::Subagent),
                    child_thread: Some(_),
                    ..
                }
            }
        ));
        assert_eq!(replay.state().tasks.len(), 1);
        assert_eq!(replay.state().tasks[0].status, ItemStatus::Completed);
        assert_eq!(
            replay.state().tasks[0].run.as_ref(),
            Some(&replay.state().runs[0].id)
        );
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn nested_subagent_model_replay_inherits_the_model_observed_in_the_owner_snapshot() {
    let replay = run("claude_nested_subagent_model", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
    assert_eq!(replay.states.len(), 3);
    for task in replay.states.values().flat_map(|s| &s.tasks) {
        assert_eq!(task.status, ItemStatus::Completed);
        assert_eq!(task.model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(
            replay.states[&task.child_thread]
                .thread
                .as_ref()
                .unwrap()
                .selection
                .model,
            "claude-haiku-4-5-20251001"
        );
        assert!(task.native_task.is_some());
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn background_subagent_resume_reuses_child_thread_and_keeps_prompt_reply_order() {
    let replay = run("claude_background_subagent_lifecycle", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed; 7]);
    assert_eq!(
        replay
            .state()
            .runs
            .iter()
            .map(|r| replay.replies(&r.id))
            .collect::<Vec<_>>()[0],
        vec!["LAUNCHED"]
    );
    assert_eq!(
        replay.replies(&replay.state().runs[1].id),
        vec!["A_REPORTED"]
    );
    assert_eq!(
        replay.replies(&replay.state().runs[2].id),
        vec!["B_STOPPED"]
    );
    assert_eq!(replay.replies(&replay.state().runs[4].id), vec!["RESUMED"]);
    assert_eq!(
        replay.replies(&replay.state().runs[5].id),
        vec!["A_RESUME_REPORTED"]
    );
    assert_eq!(replay.replies(&replay.state().runs[6].id), vec!["ALL_DONE"]);
    assert_eq!(replay.state().tasks.len(), 2);
    let agent = replay
        .state()
        .tasks
        .iter()
        .find(|t| t.native_task.as_deref() == Some("a1a715b7d0bdfefea"))
        .unwrap();
    assert_eq!(agent.status, ItemStatus::Completed);
    assert_eq!(agent.result.as_deref(), Some("A_SECOND"));
    assert_eq!(agent.run.as_ref(), Some(&replay.state().runs[4].id));
    let child = &replay.states[&agent.child_thread];
    assert_eq!(
        child
            .items
            .iter()
            .filter(|i| matches!(
                i.kind,
                ItemKind::UserMessage { .. } | ItemKind::AssistantMessage { .. }
            ))
            .map(|i| i.text.trim())
            .collect::<Vec<_>>(),
        vec![
            "Reply with exactly: A_FIRST",
            "A_FIRST",
            "Reply with exactly: A_SECOND",
            "A_SECOND"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wake_before_queued_prompt_replays_preserve_echo_and_no_echo_assignment() {
    for scenario in [
        "claude_background_wake_before_queued_prompt",
        "claude_background_wake_before_queued_prompt_no_echo",
    ] {
        let replay = run(scenario, Driver::Claude).await;
        replay.integrity();
        assert!(
            replay.statuses().iter().all(|s| *s == RunStatus::Completed),
            "{scenario}: {:?}",
            replay.statuses()
        );
        let user_runs: Vec<_> = replay
            .state()
            .runs
            .iter()
            .filter(|r| {
                replay
                    .state()
                    .messages
                    .iter()
                    .any(|m| m.id == r.message && m.created_by == MessageAuthor::User)
            })
            .collect();
        assert_eq!(user_runs.len(), 4);
        assert_eq!(replay.replies(&user_runs[1].id), vec!["B_STOPPED"]);
        if scenario.ends_with("no_echo") {
            assert_eq!(
                replay.replies(&user_runs[2].id),
                vec!["Agent B has been confirmed killed."]
            );
        } else {
            assert_eq!(replay.replies(&user_runs[0].id), vec!["LAUNCHED"]);
            assert_eq!(replay.replies(&user_runs[2].id), vec!["RESUMED"]);
            assert_eq!(replay.replies(&user_runs[3].id), vec!["ALL_DONE"]);
            let continuations: Vec<_> = replay
                .state()
                .runs
                .iter()
                .filter(|r| !user_runs.contains(r))
                .flat_map(|r| replay.replies(&r.id))
                .collect();
            assert_eq!(
                continuations,
                vec![
                    "A_REPORTED",
                    "Agent B's kill is confirmed by the notification. No further action needed — the task is complete."
                ]
            );
            let notifications: Vec<_> = replay
                .state()
                .activity_items()
                .into_iter()
                .filter_map(|i| match &i.kind {
                    ItemKind::Notification {
                        notification:
                            Notification {
                                summary, source, ..
                            },
                    } => Some((summary.clone(), source.clone())),
                    _ => None,
                })
                .collect();
            assert_eq!(
                notifications,
                vec![
                    ("Subagent \"Agent A\" finished".to_string(), NotificationSource::Native(BackgroundKind::Subagent)),
                    (
                        "Subagent \"Agent B\" and command \"Sleep 60 seconds then echo B_DONE\" were stopped".to_string(),
                        NotificationSource::Native(BackgroundKind::BackgroundTask)
                    )
                ]
            );
        }
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn idle_and_restart_replays_preserve_completed_turns_and_native_resume_identity() {
    for (scenario, expected) in [
        (
            "claude_idle_resume",
            vec![
                "idle resume first turn complete",
                "idle resume second turn complete",
            ],
        ),
        (
            "multi_turn_restart",
            vec![
                "first fixture turn complete",
                "second fixture turn complete",
            ],
        ),
    ] {
        let replay = run(scenario, Driver::Claude).await;
        assert_eq!(replay.statuses(), vec![RunStatus::Completed; 2]);
        assert_eq!(
            replay
                .state()
                .runs
                .iter()
                .flat_map(|r| replay.replies(&r.id))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            replay.state().attempts[0].native_thread,
            replay.state().attempts[1].native_thread
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_subagent_threads_refuse_messages_with_the_reference_error_and_no_projection_change()
{
    for driver in [Driver::Codex, Driver::Claude] {
        let mut replay = run("subagent", driver).await;
        let child = replay.state().tasks[0].child_thread.clone();
        let before = replay.states[&child].clone();
        let reply = replay
            .command(
                &child,
                Command::Send(SendMessage {
                    context: None,
                    created_by: MessageAuthor::User,
                    creation_source: "web".into(),
                    id: MessageId::new("message-native-child").unwrap(),
                    text: "Also check the tests.".into(),
                    attachments: vec![],
                    selection: None,
                    mode: DispatchMode::StartImmediately,
                    intent: None,
                    source_plan: None,
                    resolved_plan: None,
                    continuation: None,
                    title_seed: None,
                }),
            )
            .await;
        assert_eq!(reply,Reply::Rejected {reason:"This subagent is run by its provider and cannot take messages. Message the parent thread instead.".into()});
        assert_eq!(replay.states[&child], before);
        assert!(replay.states[&child].runs.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_provider_thread_replay_keeps_the_original_conversation_and_native_identity() {
    let replay = run("provider_thread_resume", Driver::Codex).await;
    replay.integrity();
    assert_eq!(replay.statuses(), vec![RunStatus::Completed; 2]);
    assert_eq!(
        replay
            .state()
            .runs
            .iter()
            .map(|r| r.ordinal)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        replay
            .state()
            .messages
            .iter()
            .map(|m| m.role)
            .collect::<Vec<_>>(),
        [Role::User, Role::Assistant, Role::User, Role::Assistant]
    );
    assert_eq!(replay.state().native_sessions.len(), 1);
    assert_eq!(
        replay.state().attempts[0].native_thread,
        replay.state().attempts[1].native_thread
    );
    assert_eq!(
        replay.replies(&replay.state().runs[0].id),
        ["provider thread resume fixture first turn complete"]
    );
    assert!(
        replay
            .replies(&replay.state().runs[1].id)
            .join("\n")
            .contains("provider thread resume fixture second turn complete")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_question_replay_preserves_the_native_question_id_answer_and_completed_reply() {
    let replay = run("plan_questions", Driver::Codex).await;
    replay.integrity();
    assert_eq!(replay.statuses(), [RunStatus::Completed]);
    assert_eq!(replay.state().requests.len(), 1);
    let request = &replay.state().requests[0];
    assert_eq!(request.status, RequestStatus::Resolved);
    let RequestBody::Questions { questions } = &request.body else {
        panic!()
    };
    assert_eq!(questions[0].id, "schema_preference");
    assert!(
        replay
            .replies(&replay.state().runs[0].id)
            .join("\n")
            .contains("plan questions fixture complete")
    );
    assert!(
        replay
            .state()
            .items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::UserInputRequest { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_continuation_replay_reopens_one_child_without_extra_application_runs() {
    let replay = run("subagent_continue", Driver::Codex).await;
    replay.integrity();
    assert_eq!(replay.statuses(), [RunStatus::Completed; 2]);
    assert_eq!(replay.state().tasks.len(), 1);
    let task = &replay.state().tasks[0];
    assert_eq!(task.status, ItemStatus::Completed);
    assert_eq!(task.result.as_deref(), Some("continued subagent response"));
    assert_eq!(task.run.as_ref(), Some(&replay.state().runs[1].id));
    let child = &replay.states[&task.child_thread];
    assert!(child.runs.is_empty());
    let text = child
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::UserMessage { .. } | ItemKind::AssistantMessage { .. }
            )
        })
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("initial subagent response"));
    assert!(text.contains("continued subagent response"));
    assert!(
        child
            .items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::UserMessage { .. })
                && i.text.contains("continued subagent response"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_replays_keep_the_original_outputs_without_creating_approval_requests() {
    for (scenario, driver) in [
        ("tool_call_read_only", Driver::Claude),
        ("tool_call_workspace_never", Driver::Codex),
        ("tool_call_workspace_never", Driver::Claude),
    ] {
        let replay = run(scenario, driver).await;
        replay.integrity();
        assert_eq!(replay.statuses(), [RunStatus::Completed]);
        assert!(replay.state().requests.is_empty());
        let replies = replay.replies(&replay.state().runs[0].id).join("\n");
        if scenario == "tool_call_read_only" {
            assert!(replies.contains("read only tool fixture complete"));
            let reads = replay
                .state()
                .items
                .iter()
                .filter_map(|i| {
                    if let ItemKind::DynamicTool { name, output, .. } = &i.kind {
                        Some((name, output))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 2);
            assert!(reads.iter().all(|(name, _)| name.as_str() == "Read"));
            assert!(reads.iter().any(|(_, output)| {
                serde_json::to_string(output)
                    .unwrap()
                    .contains("claude-read-only-fixture")
            }));
            assert!(
                reads
                    .iter()
                    .any(|(_, output)| serde_json::to_string(output).unwrap().contains("ESNext"))
            );
        } else {
            assert!(replies.contains("codex app-server approval fixture"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_restart_replay_preserves_interrupted_tools_and_completed_recovery() {
    let replay = run("turn_interrupt_restart", Driver::Claude).await;
    replay.integrity();
    assert_eq!(
        replay.statuses(),
        [RunStatus::Interrupted, RunStatus::Completed]
    );
    assert_eq!(
        replay
            .state()
            .attempts
            .iter()
            .map(|a| a.status)
            .collect::<Vec<_>>(),
        [AttemptStatus::Interrupted, AttemptStatus::Completed]
    );
    assert_eq!(
        replay.replies(&replay.state().runs[1].id),
        ["interrupt recovery fixture complete"]
    );
    assert!(replay.state().items.iter().any(
        |i| matches!(&i.kind,ItemKind::CommandExecution{command,..} if command.contains("node -e"))
            && i.status == ItemStatus::Interrupted
    ));
    assert_eq!(replay.state().native_sessions.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn late_background_completion_replay_clears_the_roster_and_keeps_the_root_reply() {
    let replay = run("claude_background_task_after_root", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), [RunStatus::Completed]);
    assert_eq!(replay.replies(&replay.state().runs[0].id), ["L2_STARTED"]);
    assert!(replay.state().tasks.is_empty());
    assert!(replay.state().background_work.is_empty());
    let started = replay
        .facts
        .iter()
        .position(|f| matches!(&f.body,FactBody::BackgroundTaskStarted{key,..} if key=="bc9gkn8ei"))
        .unwrap();
    let finished = replay
        .facts
        .iter()
        .position(|f| matches!(&f.body,FactBody::BackgroundTaskFinished{key} if key=="bc9gkn8ei"))
        .unwrap();
    assert!(finished > started);
}

#[tokio::test(flavor = "multi_thread")]
async fn compact_after_resumed_wake_keeps_the_unechoed_reply_on_the_compact_run() {
    let replay = run("claude_compact_after_resume_wake", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.statuses(), [RunStatus::Completed; 2]);
    assert_eq!(
        replay.replies(&replay.state().runs[0].id),
        ["compact probe first turn"]
    );
    assert_eq!(replay.replies(&replay.state().runs[1].id), ["A_REPORTED"]);
    assert!(replay.state().items.iter().any(|i| matches!(
        i.kind,
        ItemKind::Compaction {
            before: Some(27445),
            after: Some(1192)
        }
    ) && i.run.as_ref()
        == Some(&replay.state().runs[1].id)));
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_resume_after_restart_keeps_one_child_and_its_tools_and_messages() {
    let replay = run("claude_subagent_resume_after_restart", Driver::Claude).await;
    replay.integrity();
    assert_eq!(replay.state().tasks.len(), 1);
    let task = &replay.state().tasks[0];
    assert_eq!(task.status, ItemStatus::Completed);
    let child = &replay.states[&task.child_thread];
    assert!(child.runs.is_empty());
    let conversation = child
        .items
        .iter()
        .filter(|i| {
            matches!(
                i.kind,
                ItemKind::UserMessage { .. } | ItemKind::AssistantMessage { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(conversation.len(), 4);
    assert!(conversation[0].text.contains("Your job is just the file"));
    assert!(conversation[1].text.contains("is 1 line long"));
    assert!(conversation[2].text.contains("Look again at"));
    assert!(
        conversation[3]
            .text
            .contains("Bug/edge case found and fixed")
    );
    assert_eq!(
        child
            .items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::CommandExecution { .. }))
            .count(),
        3
    );
    assert!(
        !replay
            .state()
            .items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::AssistantMessage { .. })
                && i.text.contains("Bug/edge case found"))
    );
}

fn visible_text(state: &State) -> String {
    state
        .activity_items()
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ItemKind::UserMessage { .. } | ItemKind::AssistantMessage { .. }
            )
        })
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

impl Outcome {
    fn forks(&self) -> Vec<ThreadId> {
        self.replay.graph.lock().unwrap().forks.clone()
    }
}

// T3 ThreadFork.integration.test.ts: native forks keep the selected boundary,
// and sibling forks keep separate deltas.
#[tokio::test(flavor = "multi_thread")]
async fn native_fork_replays_preserve_the_selected_boundary_and_keep_sibling_deltas_separate() {
    for driver in [Driver::Codex, Driver::Claude] {
        for scenario in [
            "thread_fork_native",
            "thread_fork_native_prior_turn",
            "thread_fork_native_continue",
            "thread_fork_native_siblings",
        ] {
            let replay = run(scenario, driver).await;
            replay.integrity();
            let root = ThreadId::new("root").unwrap();
            assert_eq!(
                replay.states.len(),
                if scenario.ends_with("siblings") { 3 } else { 2 },
                "{scenario} {driver:?}"
            );
            let source = &replay.states[&root];
            for target in &replay.forks() {
                let child = &replay.states[target];
                assert_eq!(child.thread.as_ref().unwrap().parent.as_ref(), Some(&root));
                assert_eq!(child.native_sessions.len(), 1);
                assert_ne!(child.native_sessions, source.native_sessions);
                assert!(child.runs.iter().all(|r| r.status == RunStatus::Completed));
                assert_eq!(child.transfers.len(), 1);
                assert_eq!(
                    child.transfers[0].delivery.as_ref().unwrap().status,
                    ContextDeliveryStatus::NativeFork
                );
                assert!(
                    child
                        .items
                        .iter()
                        .any(|i| matches!(i.kind, ItemKind::Fork { .. }))
                );
            }
            if scenario == "thread_fork_native_prior_turn" {
                let child = &replay.states[&replay.forks()[0]];
                assert!(visible_text(child).contains("fork boundary alpha"));
                assert!(!visible_text(child).contains("fork boundary beta"));
                assert!(matches!(
                    child.inherited_items[0].kind,
                    ItemKind::UserMessage { .. }
                ));
                assert!(matches!(
                    child.inherited_items[1].kind,
                    ItemKind::AssistantMessage { .. }
                ));
            }
        }
    }
}

// T3 ProviderRollback / ThreadFork integration: rollback hides the discarded
// local items and keeps the native boundary.
#[tokio::test(flavor = "multi_thread")]
async fn rollback_replays_hide_discarded_local_items_and_preserve_the_native_boundary() {
    for (scenario, driver) in [
        ("thread_rollback", Driver::Codex),
        ("thread_rollback", Driver::Claude),
        ("thread_rollback_after_restart", Driver::Codex),
        ("thread_rollback_to_stopped_turn", Driver::Codex),
        ("thread_fork_native_fork_local_rollback", Driver::Claude),
    ] {
        let replay = run(scenario, driver).await;
        replay.integrity();
        let state = if scenario.contains("fork_local") {
            &replay.states[&replay.forks()[0]]
        } else {
            &replay.states[&ThreadId::new("root").unwrap()]
        };
        assert!(state.rollback.is_none());
        assert_eq!(
            state
                .runs
                .iter()
                .filter(|r| r.status == RunStatus::RolledBack)
                .count(),
            1,
            "{scenario} {driver:?}"
        );
        let visible = visible_text(state);
        if scenario.contains("fork_local") {
            assert!(visible.contains("fork local source alpha"));
            assert!(visible.contains("fork local first"));
            assert!(!visible.contains("fork local second"));
        } else {
            assert!(visible.contains("rollback fixture first turn complete"));
            if !scenario.contains("stopped") {
                assert!(!visible.contains("rollback fixture second turn complete"));
            }
        }
    }
}

// T3 ThreadMergeBack.integration.test.ts: each merge delivers only its fork's
// delta and the source conversation stays intact.
#[tokio::test(flavor = "multi_thread")]
async fn merge_back_replays_deliver_only_each_fork_delta_and_preserve_source_conversation() {
    for driver in [Driver::Codex, Driver::Claude] {
        for scenario in ["thread_merge_back_continue", "thread_merge_back_siblings"] {
            let replay = run(scenario, driver).await;
            replay.integrity();
            let source = &replay.states[&ThreadId::new("root").unwrap()];
            assert_eq!(
                source.transfers.len(),
                if scenario.ends_with("siblings") { 2 } else { 1 }
            );
            assert!(
                source
                    .transfers
                    .iter()
                    .all(|t| t.delivery.as_ref().is_some_and(|d| matches!(
                        d.status,
                        ContextDeliveryStatus::Inline | ContextDeliveryStatus::Injected
                    )))
            );
            let text = visible_text(source)
                .replace(" | ", "|")
                .replace("| ", "|")
                .replace(" |", "|");
            assert!(!text.contains("Context handoff ("));
            if scenario.ends_with("siblings") {
                assert!(text.contains(
                    "merge-sibling-source-3C7K|merge-sibling-first-6V2J|merge-sibling-second-9X5B"
                ));
                for (i, child) in replay.forks().iter().enumerate() {
                    let text = visible_text(&replay.states[child]);
                    assert!(text.contains(if i == 0 {
                        "first merge sibling stored"
                    } else {
                        "second merge sibling stored"
                    }));
                    assert!(!text.contains(if i == 0 {
                        "second merge sibling stored"
                    } else {
                        "first merge sibling stored"
                    }));
                }
            } else {
                assert!(text.contains("merge-source-4H8Q|merge-fork-7T2W"));
            }
            for transfer in &source.transfers {
                assert!(
                    transfer
                        .history
                        .messages
                        .iter()
                        .all(|m| m.thread == transfer.source.as_str())
                );
            }
            // T3 asserts the injected history: one fork prompt and its stored reply.
            if driver == Driver::Codex {
                let injected = replay.replay.injected.lock().unwrap().clone();
                assert_eq!(injected.len(), source.transfers.len());
                for frame in injected {
                    let items = frame["params"]["items"].as_array().unwrap();
                    // The leading coverage item is the history's context, not a message.
                    let users: Vec<_> = items[1..]
                        .iter()
                        .filter(|item| item["role"] == "user")
                        .collect();
                    assert!(
                        items[0]["content"][0]["text"]
                            .as_str()
                            .unwrap()
                            .starts_with("Context handoff (merge_back / fork_delta_summary):")
                    );
                    assert_eq!(users.len(), 1, "{frame}");
                    assert!(
                        users[0]
                            .to_string()
                            .contains("Remember the fork-local marker")
                    );
                    assert!(items.iter().any(|item| {
                        item["role"] == "assistant" && item.to_string().contains("stored")
                    }));
                }
            }
        }
    }
}

// T3 OrchestratorMcp delegated task status integration: the original result
// stays stable while follow-ups run and queue.
#[tokio::test(flavor = "multi_thread")]
async fn delegated_task_status_replay_keeps_the_original_result_while_followups_run_and_queue() {
    let mut replay = Replay::start("delegated_task_status", Driver::Codex).await;
    let rows = std::mem::take(&mut replay.rows);
    let parent = ThreadId::new("root").unwrap();
    let child = ThreadId::new("delegated-child").unwrap();
    let status = async |replay: &Replay| {
        let owner = replay.rig.state(&parent).await;
        let child = replay.rig.state(&child).await;
        delegated_task_status(
            &owner.tasks[0],
            &child.runs,
            &child.items,
            &owner.transfers,
            &child.messages,
        )
    };
    let mut turns = 0;
    let mut original = None;
    let mut original_transfer = None;
    for index in 0..rows.len() {
        let row = &rows[index];
        // The recorder stopped with the parent turn still running. Ending the
        // process instead fails that turn, and the delegated completion then
        // wakes the parent beyond the recording.
        if row["type"] == "runtime_exit" {
            continue;
        }
        replay.step(&rows, index).await;
        let frame = &row["frame"];
        if row["type"] == "expect_outbound"
            && frame["method"] == "turn/start"
            && replay.graph.lock().unwrap().delegated.is_some()
        {
            turns += 1;
        }
        if row["type"] == "expect_outbound" && frame["method"] == "turn/start" && turns == 1 {
            let state = replay.rig.state(&child).await;
            assert_eq!(
                state.thread.as_ref().unwrap().parent.as_ref(),
                Some(&parent)
            );
            assert!(state.native_owner.is_none());
        }
        if row["type"] != "emit_inbound" {
            continue;
        }
        if frame["method"] == "turn/completed" && turns == 1 {
            let status = status(&replay).await;
            original = status.child_run_id.clone();
            original_transfer = status.result_context_transfer_id.clone();
            assert_eq!(status.status, ItemStatus::Completed);
            assert_eq!(
                status.summary.as_deref(),
                Some("Delegated API boundary inspected.")
            );
            assert!(!status.has_pending_child_runs);
            assert_eq!(status.latest_terminal_run_id, original);
            assert_eq!(status.latest_terminal_status, Some(RunStatus::Completed));
            assert_eq!(status.latest_terminal_summary, status.summary);
            assert!(original_transfer.is_some());
            assert_eq!(
                status.latest_terminal_result_context_transfer_id,
                original_transfer
            );
        }
        if frame["method"] == "turn/started" && turns == 2 {
            replay
                .send_to(
                    &child,
                    "Complete the queued follow-up and return the final result.".into(),
                    false,
                )
                .await;
            replay.settle(index).await;
            let state = replay.rig.state(&child).await;
            assert_eq!(
                state.runs.iter().map(|run| run.status).collect::<Vec<_>>(),
                [RunStatus::Completed, RunStatus::Running, RunStatus::Queued]
            );
            assert_eq!(
                replay.state().await.active_run().unwrap().status,
                RunStatus::Running
            );
            let status = status(&replay).await;
            assert!(status.has_pending_child_runs);
            assert_eq!(status.child_run_id, original);
            assert_eq!(status.status, ItemStatus::Completed);
            assert_eq!(
                status.summary.as_deref(),
                Some("Delegated API boundary inspected.")
            );
            assert_eq!(status.result_context_transfer_id, original_transfer);
            assert_eq!(status.latest_terminal_run_id, original);
        }
    }
    let status = status(&replay).await;
    let state = replay.rig.state(&child).await;
    assert_eq!(status.child_run_id, original);
    assert_eq!(status.status, ItemStatus::Completed);
    assert_eq!(
        status.summary.as_deref(),
        Some("Delegated API boundary inspected.")
    );
    assert_eq!(status.result_context_transfer_id, original_transfer);
    assert!(!status.has_pending_child_runs);
    assert_eq!(
        status.latest_terminal_run_id,
        Some(state.runs[2].id.clone())
    );
    assert_eq!(status.latest_terminal_status, Some(RunStatus::Completed));
    assert_eq!(
        status.latest_terminal_summary.as_deref(),
        Some("Queued delegated follow-up completed.")
    );
    assert_eq!(status.latest_terminal_result_context_transfer_id, None);
    assert_eq!(state.runs[1].status, RunStatus::Interrupted);
    replay.finish(&rows).await.integrity();
}
