use crate::*;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

fn transcript(scenario: &str, driver: Driver) -> Vec<Value> {
    let file = match driver {
        Driver::Codex => "codex",
        Driver::Claude => "claude",
    };
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/fixtures/{scenario}/{file}_transcript.ndjson")),
    )
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

struct Replay {
    driver: Driver,
    states: BTreeMap<ThreadId, State>,
    root: ThreadId,
    codex: CodexProtocol,
    claude: ClaudeProtocol,
    prompts: VecDeque<(String, String)>,
    serial: u64,
    owner: Option<RunAttemptId>,
    facts: Vec<Fact>,
    responses: Vec<Value>,
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
            states: BTreeMap::from([(root.clone(), State::default())]),
            root: root.clone(),
            codex: CodexProtocol::default(),
            claude: ClaudeProtocol::default(),
            prompts,
            serial: 0,
            owner: None,
            facts: vec![],
            responses: vec![],
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
                    if matches!(command, ProviderCommand::Respond { .. }) {
                        self.responses.extend(output.outbound);
                    }
                    if let Some(attempt) = effect.attempt {
                        for event in output.events {
                            self.apply(
                                thread,
                                Input::Provider {
                                    attempt: attempt.clone(),
                                    event,
                                },
                            );
                        }
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
                created_by: MessageAuthor::User,
                creation_source: "web".into(),
                id: MessageId::new(format!("message-{}", self.serial)).unwrap(),
                text,
                attachments: vec![],
                selection: None,
                mode,
                intent: None,
                source_plan: None,
            }),
        );
        assert!(!matches!(reply, Reply::Rejected { .. }), "{reply:?}");
        if self.driver == Driver::Codex {
            self.owner = self.state().active_run().and_then(|r| r.attempt.clone());
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
        }
        .unwrap();
        if let Some(owner) = self.owner.clone() {
            let root = self.root.clone();
            for event in output.events {
                self.apply(
                    &root,
                    Input::Provider {
                        attempt: owner.clone(),
                        event,
                    },
                );
            }
        }
    }
    fn run(scenario: &str, driver: Driver) -> Self {
        let rows = transcript(scenario, driver);
        let mut replay = Self::new(driver, &rows);
        for row in &rows {
            let frame = &row["frame"];
            if row["type"] == "emit_inbound" {
                replay.receive(frame);
            }
            if row["type"] != "expect_outbound" {
                continue;
            }
            if driver == Driver::Codex {
                replay.codex.replay_outbound(frame);
            }
            let method = string(frame, "method");
            let kind = string(frame, "type");
            if method == "turn/start" || method == "turn/steer" || kind == "prompt.offer" {
                let text = if driver == Driver::Claude {
                    string(&frame["message"]["message"], "content")
                } else {
                    frame["params"]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                replay.send(
                    text,
                    method == "turn/steer" || frame["message"]["priority"] == "now",
                );
                if driver == Driver::Codex
                    && let Some(native_thread) = optional(&frame["params"], "threadId")
                {
                    let root = replay.root.clone();
                    let attempt = replay.owner.clone().unwrap();
                    replay.apply(
                        &root,
                        Input::Provider {
                            attempt,
                            event: ProviderEvent::SessionReady { native_thread },
                        },
                    );
                }
            } else if method == "turn/interrupt" || kind == "query.interrupt" {
                let root = replay.root.clone();
                replay.command(&root, Command::Stop);
            } else if (kind == "permission.response"
                || frame.get("result").is_some() && frame.get("id").is_some())
                && let Some(request) = replay
                    .state()
                    .requests
                    .iter()
                    .find(|r| r.status == RequestStatus::Pending)
                    .cloned()
            {
                let root = replay.root.clone();
                let decision = if frame["result"]["behavior"] == "deny" {
                    ApprovalDecision::Decline
                } else {
                    ApprovalDecision::Accept
                };
                let answers = frame["result"]["answers"].as_object().map(|a| {
                    a.iter()
                        .map(|(key, value)| {
                            (
                                key.clone(),
                                value["answers"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| v.as_str().unwrap().into())
                                    .collect(),
                            )
                        })
                        .collect()
                });
                replay.command(
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
        }
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
            .items
            .iter()
            .filter_map(|i| {
                if let ItemKind::BackgroundNotification {
                    summary,
                    outcome,
                    source,
                } = &i.kind
                {
                    Some((summary.clone(), *outcome, *source))
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
                ItemStatus::Completed,
                kind
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
fn tool_approval_replays_resolve_the_original_request_once() {
    for driver in [Driver::Codex, Driver::Claude] {
        for scenario in [
            "tool_call_read_only_on_request",
            "tool_call_restricted_granular",
        ] {
            let replay = Replay::run(scenario, driver);
            replay.integrity();
            assert_eq!(replay.statuses(), vec![RunStatus::Completed]);
            assert_eq!(replay.state().requests.len(), 1);
            assert_eq!(replay.state().requests[0].status, RequestStatus::Resolved);
            assert!(
                replay
                    .state()
                    .items
                    .iter()
                    .any(|i| matches!(i.kind, ItemKind::ApprovalRequest { .. }))
            );
        }
    }
    let replay = Replay::run("tool_call_denied_write", Driver::Claude);
    replay.integrity();
    assert_eq!(
        replay.state().requests[0].decision,
        Some(ApprovalDecision::Decline)
    );
    assert!(
        replay
            .replies(&replay.state().runs[0].id)
            .contains(&"write permission denied".into())
    );
}
