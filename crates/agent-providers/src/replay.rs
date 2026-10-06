use crate::replay_support::*;
use crate::*;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

#[test]
fn every_transcript_matches_its_digest_and_all_native_frames_decode() {
    use sha2::{Digest, Sha256};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
    let manifest: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/manifest.json")).unwrap();
    assert_eq!(manifest.len(), 71);
    for entry in manifest {
        let bytes = std::fs::read(root.join(entry["file"].as_str().unwrap())).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            string(&entry, "sha256")
        );
        let rows: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let mut codex = CodexProtocol::default();
        let mut claude = ClaudeProtocol::default();
        for (index, row) in rows.iter().enumerate() {
            if row["type"] == "expect_outbound" && entry["driver"] == "codex" {
                codex.replay_outbound(&row["frame"]);
            }
            if row["type"] != "emit_inbound" {
                continue;
            }
            let frame = &row["frame"];
            let frame = if frame["type"] == "permission.request" {
                json!({"type":"control_request","request_id":frame["options"]["toolUseID"],"request":{"subtype":"can_use_tool","tool_name":frame["toolName"],"input":frame["input"],"tool_use_id":frame["options"]["toolUseID"]}})
            } else {
                frame.clone()
            };
            let result = if entry["driver"] == "codex" {
                codex.receive(&frame)
            } else {
                claude.receive(&frame)
            };
            assert!(
                result.is_ok()
                    || matches!(result, Err(ProtocolError::Remote { .. }))
                        && frame.get("error").is_some(),
                "{} frame {index}: {result:?}",
                entry["file"]
            );
        }
    }
}

struct Replay {
    driver: Driver,
    scenario: String,
    states: BTreeMap<ThreadId, State>,
    root: ThreadId,
    codex: CodexProtocol,
    claude: ClaudeProtocol,
    prompts: VecDeque<(String, String)>,
    serial: u64,
    owner: Option<RunAttemptId>,
    facts: Vec<Fact>,
    /// Outbound frames the translators generated and the transcript has not
    /// matched yet.
    pending: VecDeque<Value>,
    context: WireContext,
    ignored_config: Vec<String>,
}
impl Replay {
    fn new(driver: Driver, rows: &[Value]) -> Self {
        let root = ThreadId::new("root").unwrap();
        let prompts = rows
            .iter()
            .filter(|row| {
                row["type"] == "expect_outbound" && row["frame"]["type"] == "prompt.offer"
            })
            .enumerate()
            .map(|(i, row)| {
                (
                    string(&row["frame"]["message"]["message"], "content"),
                    optional(&row["frame"]["message"], "uuid")
                        .unwrap_or_else(|| format!("prompt-{i}")),
                )
            })
            .collect();
        let mut replay = Self {
            driver,
            scenario: optional(&rows[0], "scenario").unwrap_or_default(),
            states: BTreeMap::from([(root.clone(), State::default())]),
            root: root.clone(),
            codex: CodexProtocol::default(),
            claude: ClaudeProtocol::default(),
            prompts,
            serial: 0,
            owner: None,
            facts: vec![],

            pending: VecDeque::new(),
            context: WireContext {
                cwd: "<workspace>".into(),
                client_name: "remote_agent_host".into(),
                client_version: "<ignored>".into(),
                ..WireContext::default()
            },
            ignored_config: rows[0]["metadata"]["recorderThreadConfigKeys"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        };
        replay.command(
            &root,
            Command::Create {
                thread: root.clone(),
                project: "project".into(),
                title: "Replay".into(),
                selection: ModelSelection {
                    instance: format!("{driver:?}"),
                    driver,
                    model: optional(&rows[0]["metadata"], "model").unwrap_or_default(),
                    options: BTreeMap::new(),
                },
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: None,
                created_by: agent_domain::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        );
        replay
    }
    fn state(&self) -> &State {
        &self.states[&self.root]
    }
    fn command(&mut self, thread: &ThreadId, command: Command) -> Reply {
        self.serial += 1;
        self.apply(
            thread,
            Input::Command {
                id: CommandId::new(format!("command-{}", self.serial)).unwrap(),
                command: Box::new(command),
                receipt: None,
            },
        )
    }
    fn apply(&mut self, thread: &ThreadId, input: Input) -> Reply {
        self.serial += 1;
        let state = self.states.entry(thread.clone()).or_default();
        let step = ThreadMachine::step(
            state,
            &InputEnvelope {
                at: Timestamp::parse("2026-10-05T00:00:00Z").unwrap(),
                key: format!("frame-{}", self.serial),
                input,
            },
        );
        *state = fold(state, &step.facts).unwrap();
        if *thread == self.root {
            self.facts.extend(step.facts);
        }
        for effect in step.effects {
            match effect.body {
                EffectBody::SendToThread { thread, command } => {
                    self.command(&thread, *command);
                }
                EffectBody::Provider(command)
                    if *thread == self.root && self.driver == Driver::Claude =>
                {
                    if matches!(
                        command,
                        ProviderCommand::Start { .. } | ProviderCommand::Compact { .. }
                    ) {
                        self.owner = effect.attempt.clone();
                    }
                    let key = if matches!(
                        command,
                        ProviderCommand::Start { .. }
                            | ProviderCommand::Compact { .. }
                            | ProviderCommand::Steer { .. }
                    ) {
                        self.prompts
                            .pop_front()
                            .map(|p| p.1)
                            .unwrap_or_else(|| format!("prompt-{}", self.serial))
                    } else {
                        String::new()
                    };
                    let output = self.claude.command(&command, &key, &[]).unwrap();
                    self.pending
                        .extend(output.outbound.iter().filter_map(sdk_frame));
                    if let Some(attempt) = effect.attempt {
                        for event in output.events {
                            self.apply(
                                thread,
                                Input::Provider {
                                    attempt: attempt.clone(),
                                    event: Box::new(event),
                                },
                            );
                        }
                    }
                }
                EffectBody::Provider(command)
                    if *thread == self.root && self.driver == Driver::Codex =>
                {
                    if matches!(
                        command,
                        ProviderCommand::Start { .. } | ProviderCommand::Compact { .. }
                    ) {
                        self.owner = effect.attempt.clone();
                    }
                    match self.codex.command(&command, &self.context, &[]) {
                        Ok(output) => {
                            self.pending.extend(output.outbound);
                            if let Some(attempt) = effect.attempt.clone() {
                                for event in output.events {
                                    self.apply(
                                        thread,
                                        Input::Provider {
                                            attempt: attempt.clone(),
                                            event: Box::new(event),
                                        },
                                    );
                                }
                            }
                        }
                        Err(ProtocolError::Remote {
                            turn_completed,
                            message,
                            ..
                        }) => {
                            let message_id = match &command {
                                ProviderCommand::Steer { message, .. } => Some(message.clone()),
                                _ => None,
                            };
                            let thread = thread.clone();
                            self.apply(
                                &thread,
                                Input::Effect(EffectResult::ProviderFailed {
                                    attempt: effect.attempt.clone().unwrap(),
                                    operation: ProviderOperation::Steer,
                                    message,
                                    message_id,
                                    turn_completed,
                                    session_lost: false,
                                }),
                            );
                        }
                        Err(error) => panic!("{}: {error}", self.scenario),
                    }
                }
                _ => {}
            }
        }
        step.reply
    }
    fn send(&mut self, text: String, steer: bool) {
        let mode = if steer {
            DispatchMode::SteerActive {
                run: self.state().active_run().unwrap().id.clone(),
            }
        } else {
            DispatchMode::QueueAfterActive
        };
        let root = self.root.clone();
        let reply = self.command(
            &root,
            Command::Send(SendMessage {
                context: None,
                created_by: MessageAuthor::User,
                creation_source: "web".into(),
                id: MessageId::new(format!("message-{}", self.serial)).unwrap(),
                text,
                attachments: vec![],
                selection: None,
                mode,
                intent: None,
                source_plan: None,
                resolved_plan: None,
                continuation: None,
                title_seed: None,
            }),
        );
        assert!(!matches!(reply, Reply::Rejected { .. }), "{reply:?}");
        if self.driver == Driver::Codex
            && !steer
            && let Some(attempt) = self.state().active_run().and_then(|r| r.attempt.clone())
        {
            self.owner = Some(attempt);
        }
    }
    fn receive(&mut self, frame: &Value) {
        let frame = if frame["type"] == "permission.request" {
            json!({"type":"control_request","request_id":frame["options"]["toolUseID"],"request":{"subtype":"can_use_tool","tool_name":frame["toolName"],"input":frame["input"],"tool_use_id":frame["options"]["toolUseID"],"permission_suggestions":frame["options"]["suggestions"],"description":frame["options"]["description"]}})
        } else {
            frame.clone()
        };
        let output = match self.driver {
            Driver::Codex => self.codex.receive(&frame),
            Driver::Claude => self.claude.receive(&frame),
        };
        let output = match output {
            Ok(output) => output,
            Err(ProtocolError::Remote { .. }) if frame.get("error").is_some() => return,
            Err(error) => panic!("{}: {error}", self.scenario),
        };
        match self.driver {
            Driver::Codex => self.pending.extend(output.outbound),
            Driver::Claude => self
                .pending
                .extend(output.outbound.iter().filter_map(sdk_frame)),
        }
        if let Some(owner) = self.owner.clone() {
            let root = self.root.clone();
            for event in output.events {
                self.apply(
                    &root,
                    Input::Provider {
                        attempt: owner.clone(),
                        event: Box::new(event),
                    },
                );
            }
        }
    }
    fn respond(&mut self, frame: &Value) {
        let Some(request) = self
            .state()
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
        let root = self.root.clone();
        self.command(
            &root,
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
        );
    }
    /// Performs the user action an expected outbound frame implies when the
    /// translators have nothing left to send.
    fn user_action(&mut self, rows: &[Value], index: usize) {
        let frame = &rows[index]["frame"];
        let method = string(frame, "method");
        let kind = string(frame, "type");
        let prompt = |frame: &Value| {
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
        };
        match (method.as_str(), kind.as_str()) {
            ("initialize", _) => {
                let frame = self.codex.initialize(&self.context);
                self.pending.push_back(frame);
            }
            ("thread/start" | "thread/resume" | "thread/inject_items" | "turn/start", _) => {
                let start = rows[index..]
                    .iter()
                    .find(|row| {
                        row["type"] == "expect_outbound" && row["frame"]["method"] == "turn/start"
                    })
                    .map(|row| prompt(&row["frame"]))
                    .unwrap_or_default();
                self.send(start, false);
                if self.scenario == "queued_turn" && self.state().runs.len() == 1 {
                    let second = rows[index + 1..]
                        .iter()
                        .filter(|row| {
                            row["type"] == "expect_outbound"
                                && row["frame"]["method"] == "turn/start"
                        })
                        .nth(1)
                        .map(|row| prompt(&row["frame"]))
                        .unwrap();
                    self.send(second, false);
                }
            }
            ("turn/steer", _) => self.send(prompt(frame), true),
            (_, "prompt.offer") => {
                let steer = frame["message"]["priority"] == "now";
                self.send(prompt(frame), steer);
                if self.scenario == "queued_turn" && self.state().runs.len() == 1 {
                    let second = rows[index + 1..]
                        .iter()
                        .find(|row| {
                            row["type"] == "expect_outbound"
                                && row["frame"]["type"] == "prompt.offer"
                        })
                        .map(|row| prompt(&row["frame"]))
                        .unwrap();
                    self.send(second, false);
                }
            }
            ("turn/interrupt" | "thread/backgroundTerminals/terminate", _)
            | (_, "query.interrupt") => {
                let root = self.root.clone();
                self.command(&root, Command::Stop);
            }
            ("thread/compact/start", _) => {
                let root = self.root.clone();
                self.command(&root, Command::Compact);
            }
            ("", "permission.response") | ("", "") if frame.get("result").is_some() => {
                self.respond(frame)
            }
            _ => {}
        }
    }
    fn expect(&mut self, rows: &[Value], index: usize) {
        if self.pending.is_empty() {
            self.user_action(rows, index);
        }
        let expected = &rows[index]["frame"];
        let mut actual = self.pending.pop_front().unwrap_or_else(|| {
            panic!(
                "{} {:?} row {index}: expected {expected} but nothing was sent",
                self.scenario, self.driver
            )
        });
        // The reference recorder offered some prompts without a message UUID.
        if expected["type"] == "prompt.offer" && expected["message"].get("uuid").is_none() {
            actual["message"]
                .as_object_mut()
                .map(|message| message.remove("uuid"));
        }
        assert_eq!(
            normalized_frame(&actual, &self.ignored_config),
            normalized_frame(expected, &self.ignored_config),
            "{} {:?} row {index}",
            self.scenario,
            self.driver
        );
    }
    /// A new CLI process: its launch must resume the session the thread holds.
    fn open_query(&mut self, frame: &Value) {
        let thread = self.state().thread.clone().unwrap();
        let instance = &thread.selection.instance;
        let launch = ClaudeLaunch {
            model: thread.selection.model.clone(),
            policy: claude_runtime_query_policy(
                thread.runtime_mode,
                thread.interaction_mode,
                None,
                None,
                false,
            ),
            native_session: self.state().native_sessions.get(instance).cloned(),
            new_session: None,
            resume_at: self.state().native_heads.get(instance).cloned().flatten(),
            fork: false,
            additional_directories: vec![],
            effort: None,
            disallowed_tools: vec![],
            mcp_servers: BTreeMap::new(),
            settings: None,
            extra_args: BTreeMap::new(),
        };
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
        let options = &frame["options"];
        assert_eq!(value("model").as_deref(), options["model"].as_str());
        assert_eq!(
            value("settings").map(|settings| serde_json::from_str::<Value>(&settings).unwrap()),
            Some(options["settings"].clone())
        );
        assert_eq!(
            value("resume").as_deref(),
            options["resume"].as_str(),
            "{}",
            self.scenario
        );
        if options["resume"].is_string() {
            assert_eq!(
                value("resume-session-at").as_deref(),
                options["resumeSessionAt"].as_str(),
                "{}",
                self.scenario
            );
        }
        self.claude = ClaudeProtocol::default();
        for state in self.states.values() {
            for task in &state.tasks {
                if let Some(native_task) = &task.native_task {
                    let parent = task.parent_task.as_ref().and_then(|id| {
                        self.states
                            .values()
                            .flat_map(|state| &state.tasks)
                            .find(|candidate| &candidate.id == id)
                            .map(|parent| parent.native_key.as_str())
                    });
                    self.claude
                        .restore_task_route(native_task, &task.native_key, parent, true);
                }
            }
            for (key, work) in &state.background_work {
                self.claude.restore_task_route(key, &work.tool, None, false);
            }
        }
        let root = self.root.clone();
        let instance = self
            .state()
            .thread
            .as_ref()
            .unwrap()
            .selection
            .instance
            .clone();
        let attempt = self
            .state()
            .active_run()
            .and_then(|run| run.attempt.clone());
        self.apply(&root, Input::RuntimeOpened { instance, attempt });
    }
    fn run(scenario: &str, driver: Driver) -> Self {
        let rows = transcript(scenario, driver);
        let mut replay = Self::new(driver, &rows);
        // The recorder's explicit runtime policy is an input of the turn.
        if let Some(start) = rows
            .iter()
            .find(|row| row["type"] == "expect_outbound" && row["frame"]["method"] == "turn/start")
        {
            let params = &start["frame"]["params"];
            if !params["approvalPolicy"].is_null() && params["approvalPolicy"] != "never" {
                replay.context.approval_policy = Some(Json(params["approvalPolicy"].clone()));
            }
            if params["sandboxPolicy"].is_object()
                && params["sandboxPolicy"]["type"] != "dangerFullAccess"
            {
                replay.context.sandbox_policy = Some(Json(params["sandboxPolicy"].clone()));
            }
            if params["collaborationMode"]["settings"]
                .get("developer_instructions")
                .is_some()
            {
                replay.context.developer_instructions = Some("<recorded>".into());
            }
            if params["collaborationMode"]["mode"] == "plan" {
                let root = replay.root.clone();
                replay.command(
                    &root,
                    Command::InteractionMode {
                        mode: InteractionMode::Plan,
                    },
                );
            }
        }
        for (index, row) in rows.iter().enumerate() {
            let frame = &row["frame"];
            match row["type"].as_str() {
                Some("emit_inbound") => replay.receive(frame),
                Some("runtime_exit") => {
                    if let Some(owner) = replay.owner.clone() {
                        let root = replay.root.clone();
                        replay.apply(
                            &root,
                            Input::Provider {
                                attempt: owner,
                                event: Box::new(ProviderEvent::SessionClosed {
                                    error: if row["status"] == "success" {
                                        None
                                    } else {
                                        Some("Provider process exited".into())
                                    },
                                }),
                            },
                        );
                    }
                    if driver == Driver::Codex {
                        replay.codex = CodexProtocol::default();
                    }
                }
                Some("expect_outbound") => match string(frame, "type").as_str() {
                    "query.open" => replay.open_query(frame),
                    "subagent.lookup" | "session.fork" => {}
                    _ => replay.expect(&rows, index),
                },
                _ => {}
            }
        }
        assert!(
            replay.pending.is_empty(),
            "{scenario} {driver:?}: unexpected outbound {:?}",
            replay.pending
        );
        replay
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
#[test]
fn queued_turn_replays_preserve_queued_acceptance_and_promotion_order() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = Replay::run("queued_turn", driver);
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
#[test]
fn steering_replays_keep_one_run_and_one_attempt() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = Replay::run("message_steering", driver);
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

#[test]
fn simple_replays_have_the_original_roles_items_ordinals_and_reply() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = Replay::run("simple", driver);
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
#[test]
fn multi_turn_replays_share_the_native_thread_and_preserve_replies() {
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = Replay::run("multi_turn", driver);
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
#[test]
fn compact_after_peer_turn_obeys_the_reference_echo_and_no_echo_paths() {
    let replay = Replay::run("claude_compact_after_peer_turn", Driver::Claude);
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
    let replay = Replay::run("claude_compact_after_peer_turn_no_echo", Driver::Claude);
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
#[test]
fn proposed_plan_and_todo_replays_keep_the_original_plan_expectations() {
    let replay = Replay::run("proposed_plan", Driver::Codex);
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
        let replay = Replay::run("todo_list", driver);
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

#[test]
fn background_command_and_monitor_replays_keep_roster_notifications_and_wake_ownership() {
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
        let replay = Replay::run(scenario, Driver::Claude);
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
#[test]
fn read_only_on_request_replays_ask_once_and_run_the_approved_write() {
    const PROBE_FILE: &str = ".codex-probe-write-action.txt";
    const PROBE_CONTENT: &str = "codex app-server approval fixture";
    for driver in [Driver::Codex, Driver::Claude] {
        let replay = Replay::run("tool_call_read_only_on_request", driver);
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
#[test]
fn restricted_granular_replays_resolve_one_request_of_the_reference_kind() {
    for (driver, kind) in [(Driver::Codex, "file-change"), (Driver::Claude, "command")] {
        let replay = Replay::run("tool_call_restricted_granular", driver);
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
#[test]
fn denied_write_replay_declines_once_and_fails_the_write() {
    let replay = Replay::run("tool_call_denied_write", Driver::Claude);
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
#[test]
fn authentication_failure_replay_preserves_failure_details_and_allows_the_followup() {
    let replay = Replay::run("claude_result_is_error", Driver::Claude);
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
#[test]
fn native_subagent_replays_keep_children_runless_and_output_out_of_the_parent() {
    for (scenario, driver, children) in [
        ("subagent", Driver::Claude, 2),
        ("subagent", Driver::Codex, 2),
        ("subagent_v2", Driver::Codex, 1),
        ("subagent_v2_nested", Driver::Codex, 3),
    ] {
        let replay = Replay::run(scenario, driver);
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

#[test]
fn stop_replays_close_attempts_tools_and_interrupt_rows() {
    for driver in [Driver::Claude, Driver::Codex] {
        for scenario in ["turn_interrupt", "turn_interrupt_mid_tool"] {
            let replay = Replay::run(scenario, driver);
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

#[test]
fn native_child_approvals_are_owned_by_the_root_and_never_appear_in_child_threads() {
    for scenario in ["subagent_v2_approval", "subagent_v2_nested_approval"] {
        let replay = Replay::run(scenario, Driver::Codex);
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
#[test]
fn local_bash_task_replay_keeps_command_output_without_child_threads() {
    let replay = Replay::run("claude_local_bash_task", Driver::Claude);
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
        matches!(&tool.kind,ItemKind::CommandExecution { command,.. } if command.contains("vp run --filter @acme/web typecheck"))
    );
    assert!(tool.text.contains("tsgo --noEmit"));
}
#[test]
fn web_search_replays_preserve_queries_and_structured_result_urls() {
    for driver in [Driver::Claude, Driver::Codex] {
        let replay = Replay::run("web_search", driver);
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
#[test]
fn mcp_tool_replay_keeps_recorded_presentation_and_leaves_absent_metadata_empty() {
    let replay = Replay::run("claude_mcp_tool_presentation", Driver::Claude);
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
#[test]
fn background_interrupt_replay_clears_the_roster_and_keeps_completed_launches() {
    let replay = Replay::run("claude_background_task_interrupt", Driver::Claude);
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

#[test]
fn background_subagent_replay_keeps_child_work_and_only_the_child_completion_wakes_root() {
    for (scenario, expected_wake) in [
        ("claude_background_subagent_after_root", "SUB_FINAL_REPORT"),
        ("claude_nested_background_subagent_wake", "CHILD_DONE"),
    ] {
        let replay = Replay::run(scenario, Driver::Claude);
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
#[test]
fn nested_subagent_model_replay_inherits_the_model_observed_in_the_owner_snapshot() {
    let replay = Replay::run("claude_nested_subagent_model", Driver::Claude);
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
#[test]
fn background_subagent_resume_reuses_child_thread_and_keeps_prompt_reply_order() {
    let replay = Replay::run("claude_background_subagent_lifecycle", Driver::Claude);
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

#[test]
fn wake_before_queued_prompt_replays_preserve_echo_and_no_echo_assignment() {
    for scenario in [
        "claude_background_wake_before_queued_prompt",
        "claude_background_wake_before_queued_prompt_no_echo",
    ] {
        let replay = Replay::run(scenario, Driver::Claude);
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
#[test]
fn idle_and_restart_replays_preserve_completed_turns_and_native_resume_identity() {
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
        let replay = Replay::run(scenario, Driver::Claude);
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

#[test]
fn native_subagent_threads_refuse_messages_with_the_reference_error_and_no_projection_change() {
    for driver in [Driver::Codex, Driver::Claude] {
        let mut replay = Replay::run("subagent", driver);
        let child = replay.state().tasks[0].child_thread.clone();
        let before = replay.states[&child].clone();
        let reply = replay.command(
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
        );
        assert_eq!(reply,Reply::Rejected {reason:"This subagent is run by its provider and cannot take messages. Message the parent thread instead.".into()});
        assert_eq!(replay.states[&child], before);
        assert!(replay.states[&child].runs.is_empty());
    }
}

#[test]
fn resumed_provider_thread_replay_keeps_the_original_conversation_and_native_identity() {
    let replay = Replay::run("provider_thread_resume", Driver::Codex);
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

#[test]
fn plan_question_replay_preserves_the_native_question_id_answer_and_completed_reply() {
    let replay = Replay::run("plan_questions", Driver::Codex);
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

#[test]
fn subagent_continuation_replay_reopens_one_child_without_extra_application_runs() {
    let replay = Replay::run("subagent_continue", Driver::Codex);
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

#[test]
fn tool_replays_keep_the_original_outputs_without_creating_approval_requests() {
    for (scenario, driver) in [
        ("tool_call_read_only", Driver::Claude),
        ("tool_call_workspace_never", Driver::Codex),
        ("tool_call_workspace_never", Driver::Claude),
    ] {
        let replay = Replay::run(scenario, driver);
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

#[test]
fn interrupt_restart_replay_preserves_interrupted_tools_and_completed_recovery() {
    let replay = Replay::run("turn_interrupt_restart", Driver::Claude);
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

#[test]
fn late_background_completion_replay_clears_the_roster_and_keeps_the_root_reply() {
    let replay = Replay::run("claude_background_task_after_root", Driver::Claude);
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

#[test]
fn compact_after_resumed_wake_keeps_the_unechoed_reply_on_the_compact_run() {
    let replay = Replay::run("claude_compact_after_resume_wake", Driver::Claude);
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

#[test]
fn subagent_resume_after_restart_keeps_one_child_and_its_tools_and_messages() {
    let replay = Replay::run("claude_subagent_resume_after_restart", Driver::Claude);
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

#[test]
fn stopped_root_accepts_its_child_confirmation_and_native_deletion_cannot_strand_the_task() {
    for delete in [false, true] {
        let mut replay = Replay::new(Driver::Claude, &[json!({"metadata":{"model":"model"}})]);
        replay.send("root prompt".into(), false);
        let root = replay.root.clone();
        let owner = replay.owner.clone().unwrap();
        replay.apply(
            &root,
            Input::Provider {
                attempt: owner.clone(),
                event: Box::new(ProviderEvent::SubagentStarted {
                    background: true,
                    native_thread: None,
                    key: "child".into(),
                    parent: None,
                    prompt: "child prompt".into(),
                    model: None,
                }),
            },
        );
        let child = replay.state().tasks[0].child_thread.clone();
        replay.apply(
            &root,
            Input::Provider {
                attempt: owner.clone(),
                event: Box::new(ProviderEvent::Child {
                    key: "child".into(),
                    event: Box::new(ProviderEvent::TextDelta {
                        key: "text".into(),
                        kind: ProviderItem::Text,
                        text: "child output".into(),
                    }),
                }),
            },
        );
        if delete {
            assert_eq!(replay.command(&child, Command::Delete), Reply::Accepted);
            assert_eq!(replay.state().tasks[0].status, ItemStatus::Cancelled);
            let before = replay.states[&child].clone();
            assert_eq!(
                replay.apply(
                    &child,
                    Input::Provider {
                        attempt: owner,
                        event: Box::new(ProviderEvent::TextDelta {
                            key: "late".into(),
                            kind: ProviderItem::Text,
                            text: "late output".into()
                        }),
                    }
                ),
                Reply::Ignored
            );
            assert_eq!(replay.states[&child], before);
        } else {
            replay.command(&root, Command::Stop);
            replay.apply(
                &root,
                Input::Provider {
                    attempt: owner.clone(),
                    event: Box::new(ProviderEvent::TurnFinished {
                        status: RunStatus::Interrupted,
                        native_head: None,
                    }),
                },
            );
            assert_eq!(replay.state().tasks[0].status, ItemStatus::Running);
            replay.apply(
                &root,
                Input::Provider {
                    attempt: owner,
                    event: Box::new(ProviderEvent::NativeOutput {
                        echoed_prompts: vec![],
                        acknowledged_prompt: None,
                        root: false,
                        result: None,
                        events: vec![ProviderEvent::Child {
                            key: "child".into(),
                            event: Box::new(ProviderEvent::SessionClosed { error: None }),
                        }],
                    }),
                },
            );
            assert_eq!(replay.state().tasks[0].status, ItemStatus::Interrupted);
            assert!(
                replay.states[&child]
                    .items
                    .iter()
                    .all(|item| item.status.terminal())
            );
        }
    }
}
