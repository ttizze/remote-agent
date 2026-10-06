use crate::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct WireContext {
    /// The app thread a command belongs to; one app-server serves many.
    pub route: String,
    pub cwd: String,
    pub client_name: String,
    pub client_version: String,
    pub approval_policy: Option<Json>,
    pub sandbox_policy: Option<Json>,
    pub developer_instructions: Option<String>,
    pub additional_context: Option<Json>,
    pub omit_service_tier: bool,
    pub thread_model: Option<String>,
    pub thread_config: BTreeMap<String, Json>,
}
impl WireContext {
    fn thread_params(&self, model: Option<&str>) -> Value {
        let mut config = json!(self.thread_config);
        config["tools.update_plan.enabled"] = json!(true);
        let mut params = json!({"cwd":self.cwd,"config":config});
        if let Some(model) = model {
            params["model"] = json!(model);
        }
        params
    }
    /// T3's provider-thread load key: a thread loaded for another selection or
    /// runtime policy is resumed with the new parameters before its turn.
    fn load_key(
        &self,
        selection: &ModelSelection,
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
    ) -> String {
        json!([
            selection,
            runtime_mode,
            interaction_mode,
            self.cwd,
            self.approval_policy,
            self.sandbox_policy
        ])
        .to_string()
    }
}
/// One app thread's view of the shared app-server.
#[derive(Debug, Default)]
struct Route {
    /// The root native thread loaded for this route, and its load key.
    thread: Option<String>,
    load_key: Option<String>,
    stop_before_thread: bool,
}
#[derive(Debug, Clone)]
enum Pending {
    Initialize,
    Thread {
        /// The resume parameters; `None` starts a new thread.
        resume: Option<Resume>,
        then: Option<Then>,
        load_key: Option<String>,
    },
    /// `thread/unarchive` before resuming an archived session again.
    Unarchive {
        params: Value,
        then: Option<Then>,
        load_key: Option<String>,
    },
    Inject {
        start: Value,
        history: InlineHistory,
    },
    /// Restores a thread's additional context after a compaction.
    Restore,
    /// Drops this connection's subscription to a thread another route no longer uses.
    Unload,
    /// T3 revertCodexThread: at most `turns` newest turns after the target's
    /// boundary are discarded.
    RevertRead {
        target: RevertTarget,
        turns: u64,
        runtime_params: Value,
    },
    RevertResume {
        target: RevertTarget,
        turns: u64,
    },
    RevertPage {
        target: RevertTarget,
        remaining: u64,
        before: Option<String>,
        visited: BTreeSet<Option<String>>,
    },
    /// The final `thread/revert`, or `thread/read` when no turn was listed.
    Revert {
        thread: String,
        read: bool,
    },
    Terminate {
        thread: String,
        process: String,
    },
    TerminalList {
        thread: String,
        process: String,
    },
    Operation(String),
}
/// The native thread a rollback reverts and the turn it keeps last; `None`
/// keeps no turn.
#[derive(Debug, Clone)]
struct RevertTarget {
    thread: String,
    boundary: Option<String>,
}
#[derive(Debug, Clone)]
struct Resume {
    params: Value,
    unarchived: bool,
}
/// What runs once the native thread is started or resumed.
#[derive(Debug, Clone)]
enum Then {
    Turn {
        start: Value,
        history: Option<InlineHistory>,
    },
    Compact,
}
impl Pending {
    fn operation(&self) -> String {
        match self {
            Pending::Initialize => "initialize".into(),
            Pending::Thread {
                resume: Some(_), ..
            }
            | Pending::Unarchive { .. } => "thread/resume".into(),
            Pending::Thread { .. } => "thread/start".into(),
            Pending::Inject { .. } => "thread/inject_items".into(),
            Pending::Restore => "additional context restore".into(),
            Pending::Unload => "thread/unsubscribe".into(),
            Pending::RevertRead { .. } => "thread/read".into(),
            Pending::RevertResume { .. } => "thread/resume".into(),
            Pending::RevertPage { .. } => "thread/turns/list".into(),
            Pending::Revert { read: true, .. } => "thread/read".into(),
            Pending::Revert { .. } => "thread/revert".into(),
            Pending::Terminate { .. } => "thread/backgroundTerminals/terminate".into(),
            Pending::TerminalList { .. } => "thread/backgroundTerminals/list".into(),
            Pending::Operation(operation) => operation.clone(),
        }
    }
}
/// A command execution the native turn has not completed.
#[derive(Debug, Clone)]
struct RunningCommand {
    thread: String,
    turn: String,
    command: String,
    output: String,
    process: Option<String>,
}
/// Final answers of one native turn: later duplicates and empty completions
/// after an answer are not new answers.
#[derive(Debug, Default)]
struct FinalAnswers {
    first: Option<String>,
    texts: BTreeSet<String>,
}
/// Historical context for injection, and the complete prompt text used when
/// the native thread cannot inject it.
#[derive(Debug, Clone)]
struct InlineHistory {
    history: HistoricalContext,
    inline_text: String,
}
/// One app-server process's RPC correlation, with no application entities. The
/// native threads of many app threads (routes) share its JSON-RPC id space.
#[derive(Debug, Default)]
pub struct CodexProtocol {
    next_id: u64,
    pending: BTreeMap<u64, (String, Pending)>,
    routes: BTreeMap<String, Route>,
    /// Root native thread -> route.
    roots: BTreeMap<String, String>,
    /// Native threads loaded without a route yet, such as a fork's thread.
    unclaimed: BTreeSet<String>,
    turns: BTreeMap<String, String>,
    reasoning_parts: BTreeMap<String, BTreeMap<(String, u64), String>>,
    interrupt_pending: BTreeSet<String>,
    /// Native child thread -> native parent (empty under a root), used solely
    /// to route notifications.
    children: BTreeMap<String, String>,
    /// Native child thread -> its root native thread.
    child_roots: BTreeMap<String, String>,
    /// Pending server request ID -> method, to shape the reply.
    server_requests: BTreeMap<String, String>,
    /// The additional context each native thread last received; restored after compaction.
    additional_context: BTreeMap<String, Value>,
    running: BTreeMap<String, RunningCommand>,
    /// Completed turns that still own running commands.
    settled: BTreeSet<String>,
    /// Threads whose work the user stopped; their late completions do not wake.
    stopping: BTreeSet<String>,
    final_answers: BTreeMap<String, FinalAnswers>,
    deferred: BTreeSet<String>,
    async_messages: BTreeSet<String>,
    retries: BTreeMap<String, RetryProgress>,
    failures: BTreeMap<String, (String, Option<String>, String)>,
    /// The account's rate-limit snapshot, merged across partial updates.
    rate_limits: Option<Value>,
}
impl CodexProtocol {
    fn request(&mut self, route: &str, method: &str, params: Value, pending: Pending) -> Value {
        self.next_id += 1;
        self.pending.insert(self.next_id, (route.into(), pending));
        json!({"id":self.next_id,"method":method,"params":params})
    }
    pub fn initialize(&mut self, context: &WireContext) -> Value {
        self.request("", "initialize",json!({"capabilities":{"experimentalApi":true,"optOutNotificationMethods":["turn/diff/updated"]},"clientInfo":{"name":context.client_name,"title":context.client_name,"version":context.client_version}}),Pending::Initialize)
    }
    /// `account/login/start` for the app-server's managed account.
    pub fn login(&mut self, params: Value) -> Value {
        self.request(
            "",
            "account/login/start",
            params,
            Pending::Operation("account/login/start".into()),
        )
    }
    fn route_mut(&mut self, route: &str) -> &mut Route {
        self.routes.entry(route.into()).or_default()
    }
    /// The root native thread of the route, if loaded.
    pub fn native_thread(&self, route: &str) -> Option<&str> {
        self.routes.get(route)?.thread.as_deref()
    }
    /// The route of a root or child native thread. A process serving a single
    /// route attributes unknown threads to it, but not a fork's thread no
    /// route has claimed yet.
    fn route_of(&self, native: Option<&str>) -> Option<String> {
        if native.is_some_and(|native| self.unclaimed.contains(native)) {
            return None;
        }
        let found = native.and_then(|native| {
            let root = self
                .child_roots
                .get(native)
                .map(String::as_str)
                .unwrap_or(native);
            self.roots.get(root).cloned()
        });
        found.or_else(|| match self.routes.len() {
            1 => self.routes.keys().next().cloned(),
            _ => None,
        })
    }
    fn is_root(&self, native: &str) -> bool {
        self.roots.contains_key(native)
    }
    /// Records a native child under the notification's thread.
    fn adopt_child(&mut self, child: &str, parent: Option<&String>) {
        let parent = parent.cloned().unwrap_or_default();
        let root = if self.is_root(&parent) {
            Some(parent.clone())
        } else {
            self.child_roots.get(&parent).cloned()
        };
        if let Some(root) = root {
            self.child_roots.entry(child.into()).or_insert(root);
        }
        let under = if self.is_root(&parent) {
            String::new()
        } else {
            parent
        };
        self.children.entry(child.into()).or_insert(under);
    }
    fn bind_root(&mut self, route: &str, thread: &str, load_key: Option<String>) {
        self.unclaimed.remove(thread);
        self.roots.insert(thread.into(), route.into());
        let state = self.route_mut(route);
        state.thread = Some(thread.into());
        state.load_key = load_key;
    }
    /// Stops using the route's native thread in this process (T3 unloadThread):
    /// the app-server may shut it down, and a later turn resumes it.
    pub fn unload(&mut self, route: &str) -> Translation {
        let Some(thread) = self.routes.get_mut(route).and_then(|r| {
            r.load_key = None;
            r.thread.take()
        }) else {
            return Translation::default();
        };
        Translation {
            outbound: vec![self.request(
                route,
                "thread/unsubscribe",
                json!({"threadId":thread}),
                Pending::Unload,
            )],
            route: Some(route.into()),
            ..Translation::default()
        }
    }
    /// Interrupts the route's running root turn, as T3 does for a thread that
    /// detaches from a shared app-server.
    pub fn interrupt_active_turn(&mut self, route: &str) -> Translation {
        let Some(thread) = self.native_thread(route).map(str::to_owned) else {
            return Translation::default();
        };
        let Some(turn) = self.turns.get(&thread).cloned() else {
            return Translation::default();
        };
        self.stopping.insert(thread.clone());
        Translation {
            outbound: vec![self.request(
                route,
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn}),
                Pending::Operation("turn/interrupt".into()),
            )],
            route: Some(route.into()),
            ..Translation::default()
        }
    }
    /// Ends the route's root turn as interrupted when the app-server did not
    /// (T3 finalizeRemainingInterruptLineage after its interrupt timeout).
    pub fn abandon_turn(&mut self, route: &str) -> Translation {
        let mut output = Translation {
            route: Some(route.into()),
            ..Translation::default()
        };
        let thread = self.native_thread(route).map(str::to_owned);
        if let Some(thread) = thread
            && let Some(turn) = self.turns.get(&thread).cloned()
        {
            let completed = json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"status":"interrupted"}}});
            output.events = self
                .receive(&completed)
                .map(|translation| translation.events)
                .unwrap_or_default();
        } else if self.turn_in_flight(route) {
            self.pending.retain(|_, (owner, _)| owner != route);
            output.events.push(ProviderEvent::TurnFinished {
                status: RunStatus::Interrupted,
                native_head: None,
            });
        }
        output
    }
    /// Image bytes are prepared by the resource owner; paths are never sent.
    pub fn command(
        &mut self,
        command: &ProviderCommand,
        context: &WireContext,
        images: &[PreparedImage],
    ) -> Result<Translation, ProtocolError> {
        let mut output = self.command_frames(command, context, images)?;
        output.route = Some(context.route.clone());
        Ok(output)
    }
    fn command_frames(
        &mut self,
        command: &ProviderCommand,
        context: &WireContext,
        images: &[PreparedImage],
    ) -> Result<Translation, ProtocolError> {
        let route = context.route.as_str();
        let frame = match command {
            ProviderCommand::Start {
                resume_interrupted_turn,
                selection,
                runtime_mode,
                interaction_mode,
                text,
                note,
                attachments,
                native_thread,
                context: handoff,
                ..
            } => {
                let input = if *resume_interrupted_turn {
                    vec![]
                } else {
                    codex_input(
                        &provider_prompt(text, note.as_deref(), None),
                        attachments,
                        images,
                    )?
                };
                let handoff = handoff.as_ref().map(|history| InlineHistory {
                    history: history.clone(),
                    inline_text: attachment_text(
                        &codex_skill_mention_text(&provider_prompt(
                            text,
                            note.as_deref(),
                            Some(history),
                        )),
                        attachments,
                    ),
                });
                let (approval, reviewer, sandbox) = codex_runtime(*runtime_mode);
                let mut start = json!({"input":input,"cwd":context.cwd,"model":selection.model,"summary":"detailed","approvalPolicy":approval,"approvalsReviewer":reviewer,"sandboxPolicy":{"type":sandbox}});
                if let Some(policy) = &context.approval_policy {
                    start["approvalPolicy"] = policy.0.clone();
                }
                if let Some(policy) = &context.sandbox_policy {
                    start["sandboxPolicy"] = policy.0.clone();
                }
                if let Some(additional_context) = &context.additional_context {
                    start["additionalContext"] = additional_context.0.clone();
                }
                if let Some(effort) = selection.options.get("reasoningEffort") {
                    start["effort"] = json!(effort);
                }
                if let Some(tier) = selection.options.get("serviceTier")
                    && !context.omit_service_tier
                {
                    start["serviceTier"] = json!(tier);
                }
                if *interaction_mode == InteractionMode::Plan
                    || context.developer_instructions.is_some()
                {
                    let mut settings = json!({"model":selection.model,"reasoning_effort":selection.options.get("reasoningEffort").map(String::as_str).unwrap_or("medium")});
                    if let Some(instructions) = &context.developer_instructions {
                        settings["developer_instructions"] = json!(instructions);
                    }
                    start["collaborationMode"] = json!({"mode":if *interaction_mode == InteractionMode::Plan {"plan"} else {"default"},"settings":settings});
                }
                let load_key = context.load_key(selection, *runtime_mode, *interaction_mode);
                if native_thread
                    .as_ref()
                    .is_some_and(|thread| self.unclaimed.contains(thread))
                {
                    // A fork loaded the thread in this process for its first turn.
                    let thread = native_thread.clone().unwrap_or_default();
                    self.bind_root(route, &thread, Some(load_key.clone()));
                }
                let state = self.route_mut(route);
                if let Some(thread) = state.thread.clone().filter(|id| {
                    native_thread.as_ref() == Some(id)
                        && state.load_key.as_ref().is_none_or(|key| *key == load_key)
                }) {
                    state.load_key = Some(load_key);
                    start["threadId"] = json!(thread);
                    self.start_or_inject(route, start, handoff)
                } else if let Some(thread) = native_thread {
                    let mut params = context.thread_params(Some(&selection.model));
                    params["threadId"] = json!(thread);
                    params["excludeTurns"] = json!(true);
                    self.request(
                        route,
                        "thread/resume",
                        params.clone(),
                        Pending::Thread {
                            resume: Some(Resume {
                                params,
                                unarchived: false,
                            }),
                            then: Some(Then::Turn {
                                start,
                                history: handoff,
                            }),
                            load_key: Some(load_key),
                        },
                    )
                } else {
                    self.request(
                        route,
                        "thread/start",
                        context.thread_params(Some(&selection.model)),
                        Pending::Thread {
                            resume: None,
                            then: Some(Then::Turn {
                                start,
                                history: handoff,
                            }),
                            load_key: Some(load_key),
                        },
                    )
                }
            }
            ProviderCommand::Steer {
                text, attachments, ..
            } => {
                let thread = self
                    .native_thread(route)
                    .ok_or_else(|| ProtocolError::Invalid("no native thread".into()))?
                    .to_owned();
                let thread = &thread;
                let turn = self
                    .turns
                    .get(thread)
                    .ok_or_else(|| ProtocolError::Remote {
                        request: None,
                        operation: "turn/steer".into(),
                        message: "No active turn".into(),
                        turn_completed: true,
                    })?;
                let input = codex_input(text, attachments, images)?;
                let params = json!({"threadId":thread,"expectedTurnId":turn,"input":input});
                self.request(
                    route,
                    "turn/steer",
                    params,
                    Pending::Operation("turn/steer".into()),
                )
            }
            ProviderCommand::Interrupt {
                native_thread,
                native_turn,
            } => {
                let mut output = Translation::default();
                let own = |pending: &&(String, Pending)| pending.0 == route;
                if self.pending.values().filter(own).any(|(_, p)| {
                    matches!(
                        p,
                        Pending::Thread { then: Some(_), .. } | Pending::Unarchive { .. }
                    )
                }) {
                    self.route_mut(route).stop_before_thread = true;
                }
                let thread = native_thread
                    .clone()
                    .or_else(|| self.native_thread(route).map(str::to_owned));
                if let Some(thread) = thread {
                    // A completed turn is not interrupted again; only its
                    // retained commands are stopped.
                    let active = self.turns.get(&thread).cloned();
                    let turn = match native_turn {
                        Some(turn) => active.filter(|active| active == turn),
                        None => active,
                    };
                    if self.pending.values().any(|(_, pending)| matches!(pending,Pending::Inject {start,..} if start["threadId"] == thread)) {
                        self.route_mut(route).stop_before_thread = true;
                    }
                    if turn.is_none()
                        && self
                            .pending
                            .values()
                            .any(|(_, p)| matches!(p, Pending::Operation(op) if op == "turn/start"))
                    {
                        self.interrupt_pending.insert(thread.clone());
                    }
                    self.stopping.insert(thread.clone());
                    output.outbound = self.interrupt(route, &thread, turn.as_deref());
                    let untracked = self
                        .running
                        .iter()
                        .filter(|(_, command)| {
                            command.thread == thread
                                && command.process.is_none()
                                && self.settled.contains(&command.turn)
                        })
                        .map(|(item, _)| item.clone())
                        .collect::<Vec<_>>();
                    for item in untracked {
                        output.events.extend(self.stop_command(&item));
                    }
                }
                return Ok(output);
            }
            ProviderCommand::Respond {
                native_key,
                decision,
                answers,
                input,
            } => {
                let id: Value = serde_json::from_str(native_key)
                    .map_err(|_| ProtocolError::Invalid("invalid native request id".into()))?;
                let question = self.server_requests.remove(native_key).as_deref()
                    == Some("item/tool/requestUserInput");
                let result = if question {
                    json!({"answers":answers.iter().flatten().map(|(k,v)| (k.clone(),json!({"answers":v.choices()}))).collect::<BTreeMap<_,_>>()})
                } else if let Some(answers) = answers {
                    json!({"answers":answers.iter().map(|(k,v)| (k.clone(),json!({"answers":v.choices()}))).collect::<BTreeMap<_,_>>()})
                } else if let Some(payload) = input
                    .as_ref()
                    .filter(|input| input.0.get("permissions").is_some())
                {
                    let decision = decision.unwrap_or(ApprovalDecision::Cancel);
                    if matches!(
                        decision,
                        ApprovalDecision::Accept | ApprovalDecision::AcceptForSession
                    ) {
                        json!({"permissions":payload.0["permissions"],"scope":if decision==ApprovalDecision::AcceptForSession {"session"} else {"turn"}})
                    } else {
                        json!({"permissions":{},"scope":"turn"})
                    }
                } else if let Some(payload) = input.as_ref().filter(|input| {
                    input.0.get("serverName").is_some()
                        && (input.0.get("requestedSchema").is_some()
                            || input.0.get("mode").is_some())
                }) {
                    mcp_elicitation_response(
                        &payload.0,
                        decision.unwrap_or(ApprovalDecision::Cancel),
                    )
                } else {
                    json!({"decision":match decision.unwrap_or(ApprovalDecision::Cancel) { ApprovalDecision::Accept=>"accept",ApprovalDecision::AcceptForSession|ApprovalDecision::AcceptAlways=>"acceptForSession",ApprovalDecision::Decline=>"decline",ApprovalDecision::Cancel=>"cancel" }})
                };
                json!({"id":id,"result":result})
            }
            ProviderCommand::Rollback { .. } => {
                return Err(ProtocolError::Invalid(
                    "a Codex rollback names the number of turns to discard".into(),
                ));
            }
            ProviderCommand::Fork {
                native_thread,
                through_turn,
            } => {
                let mut params = context.thread_params(context.thread_model.as_deref());
                params["threadId"] = json!(native_thread);
                if let Some(turn) = through_turn {
                    params["lastTurnId"] = json!(turn);
                }
                self.request(
                    route,
                    "thread/fork",
                    params,
                    Pending::Operation("thread/fork".into()),
                )
            }
            ProviderCommand::Compact {
                native_thread: Some(thread),
            } if self.native_thread(route) != Some(thread.as_str()) => {
                let mut params = context.thread_params(context.thread_model.as_deref());
                params["threadId"] = json!(thread);
                params["excludeTurns"] = json!(true);
                self.request(
                    route,
                    "thread/resume",
                    params.clone(),
                    Pending::Thread {
                        resume: Some(Resume {
                            params,
                            unarchived: false,
                        }),
                        then: Some(Then::Compact),
                        load_key: None,
                    },
                )
            }
            ProviderCommand::Compact { .. } => match self.native_thread(route).map(str::to_owned) {
                Some(thread) => self.compact(route, &thread),
                None => self.request(
                    route,
                    "thread/start",
                    context.thread_params(context.thread_model.as_deref()),
                    Pending::Thread {
                        resume: None,
                        then: Some(Then::Compact),
                        load_key: None,
                    },
                ),
            },
        };
        Ok(Translation {
            outbound: vec![frame],
            ..Translation::default()
        })
    }
    /// Ends a retained command that the user stopped.
    fn stop_command(&mut self, item: &str) -> Vec<ProviderEvent> {
        let Some(command) = self.running.remove(item) else {
            return vec![];
        };
        let events = vec![
            ProviderEvent::ItemFinished {
                key: item.into(),
                kind: ProviderItem::Command {
                    command: command.command.clone(),
                    cwd: None,
                    exit_code: None,
                },
                text: (!command.output.is_empty()).then_some(command.output),
                status: ItemStatus::Interrupted,
            },
            ProviderEvent::BackgroundTask {
                key: item.into(),
                tool: item.into(),
                kind: BackgroundKind::Command,
                description: command.command,
                status: Some(ItemStatus::Cancelled),
                summary: None,
                exit_code: None,
            },
        ];
        self.release_settled(&command.turn);
        self.route(events, Some(&command.thread))
    }
    fn release_settled(&mut self, turn: &str) {
        if !self.running.values().any(|command| command.turn == turn) {
            self.settled.remove(turn);
        }
    }
    /// A retry item resolves when the provider produces output again.
    fn resolve_retry(&mut self, turn: &str, status: ItemStatus) -> Option<ProviderEvent> {
        let retry = self.retries.remove(turn)?;
        let (message, code, class) = self.failures.get(turn).cloned().unwrap_or_default();
        Some(ProviderEvent::ItemFinished {
            key: format!("terminal-failure:{turn}"),
            kind: ProviderItem::Error {
                message,
                retry: Some(retry),
                code,
                class: Some(class),
                retryable: Some(true),
            },
            text: None,
            status,
        })
    }
    fn route(&self, events: Vec<ProviderEvent>, thread: Option<&String>) -> Vec<ProviderEvent> {
        match thread.filter(|thread| !self.is_root(thread)) {
            Some(thread) => child_events(events, thread, &self.children).unwrap_or_default(),
            None => events,
        }
    }
    fn process_item(&self, process: &str) -> Option<String> {
        self.running
            .iter()
            .find(|(_, command)| command.process.as_deref() == Some(process))
            .map(|(item, _)| item.clone())
    }
    /// T3 CodexAdapterV2 rollbackThread: discards the `turns` newest turns of
    /// the native thread. No turn to discard sends nothing.
    /// Reverts to just after the `boundary` turn. Listing the boundary ends the
    /// count, so repeating a rollback leaves an already rewound thread as it is.
    pub fn rollback(
        &mut self,
        native_thread: &str,
        turns: u64,
        boundary: Option<&str>,
        context: &WireContext,
    ) -> Translation {
        let route = context.route.as_str();
        let mut output = Translation {
            route: Some(route.into()),
            ..Translation::default()
        };
        if turns == 0 {
            output.completion = Some(Completion::RolledBack {
                native_thread: native_thread.into(),
            });
            return output;
        }
        output.outbound.push(self.request(
            route,
            "thread/read",
            json!({"threadId":native_thread,"includeTurns":false}),
            Pending::RevertRead {
                target: RevertTarget {
                    thread: native_thread.into(),
                    boundary: boundary.map(str::to_owned),
                },
                turns,
                runtime_params: context.thread_params(context.thread_model.as_deref()),
            },
        ));
        output
    }
    fn revert_page(
        &mut self,
        route: &str,
        target: RevertTarget,
        remaining: u64,
        before: Option<String>,
        cursor: Option<String>,
        mut visited: BTreeSet<Option<String>>,
    ) -> Result<Value, ProtocolError> {
        if !visited.insert(cursor.clone()) {
            return Err(ProtocolError::Invalid(
                "Thread history pagination repeated a cursor.".into(),
            ));
        }
        Ok(self.request(route, "thread/turns/list",json!({"threadId":target.thread,"cursor":cursor,"limit":remaining.min(100),"sortDirection":"desc","itemsView":"summary"}),Pending::RevertPage {target,remaining,before,visited}))
    }
    fn start_or_inject(
        &mut self,
        route: &str,
        start: Value,
        history: Option<InlineHistory>,
    ) -> Value {
        if let Some(context) = start.get("additionalContext")
            && let Some(thread) = start["threadId"].as_str()
        {
            self.additional_context
                .insert(thread.into(), context.clone());
        } else if let Some(thread) = start["threadId"].as_str() {
            self.additional_context.remove(thread);
        }
        if let Some(history) = history {
            self.request(route, "thread/inject_items",json!({"threadId":start["threadId"],"items":history_response_items(&history.history.messages,&history.history.context)}),Pending::Inject {start,history})
        } else {
            self.turn_start(route, start)
        }
    }
    fn turn_start(&mut self, route: &str, start: Value) -> Value {
        self.request(
            route,
            "turn/start",
            start,
            Pending::Operation("turn/start".into()),
        )
    }
    fn compact(&mut self, route: &str, thread: &str) -> Value {
        self.request(
            route,
            "thread/compact/start",
            json!({"threadId":thread}),
            Pending::Operation("thread/compact/start".into()),
        )
    }
    /// T3 restoreAdditionalContext: Codex drops client developer messages when it
    /// compacts but resends additional context only when it changes.
    fn restore_additional_context(&mut self, thread: &str) -> Option<Value> {
        let context = self.additional_context.get(thread)?.as_object()?.clone();
        let route = self.route_of(Some(thread)).unwrap_or_default();
        let items = context
            .iter()
            .map(|(key, entry)| {
                let value = entry["value"].as_str().unwrap_or_default();
                json!({"type":"message","role":"developer","content":[{"type":"input_text","text":format!("<{key}>{value}</{key}>")}]})
            })
            .collect::<Vec<_>>();
        Some(self.request(
            &route,
            "thread/inject_items",
            json!({"threadId":thread,"items":items}),
            Pending::Restore,
        ))
    }
    fn interrupt(&mut self, route: &str, thread: &str, turn: Option<&str>) -> Vec<Value> {
        let mut outbound = vec![];
        if let Some(turn) = turn {
            outbound.push(self.request(
                route,
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn}),
                Pending::Operation("turn/interrupt".into()),
            ));
        }
        let processes = self
            .running
            .values()
            .filter(|command| command.thread == thread)
            .filter_map(|command| command.process.clone())
            .collect::<BTreeSet<_>>();
        for process in processes {
            outbound.push(self.request(
                route,
                "thread/backgroundTerminals/terminate",
                json!({"threadId":thread,"processId":process}),
                Pending::Terminate {
                    thread: thread.into(),
                    process,
                },
            ));
        }
        outbound
    }
    /// A root turn of the route runs, or a sent request will start one.
    pub fn turn_in_flight(&self, route: &str) -> bool {
        self.native_thread(route)
            .is_some_and(|thread| self.turns.contains_key(thread))
            || self
                .pending
                .values()
                .filter(|(owner, _)| owner == route)
                .any(|(_, pending)| match pending {
                    Pending::Thread { then, .. } | Pending::Unarchive { then, .. } => {
                        then.is_some()
                    }
                    Pending::Inject { .. } => true,
                    Pending::Operation(operation) => {
                        operation == "turn/start" || operation == "thread/compact/start"
                    }
                    _ => false,
                })
    }
    pub fn receive(&mut self, frame: &Value) -> Result<Translation, ProtocolError> {
        let mut output = Translation::default();
        if frame.get("method").is_none() {
            let Some(id) = frame.get("id").and_then(Value::as_u64) else {
                return Ok(output);
            };
            let Some((route, pending)) = self.pending.remove(&id) else {
                return Ok(output);
            };
            output.route = Some(route.clone());
            if let Some(error) = frame.get("error") {
                if error["code"] == -32601
                    && let Pending::Inject { mut start, history } = pending
                {
                    if std::mem::take(&mut self.route_mut(&route).stop_before_thread) {
                        return Ok(output);
                    }
                    replace_input_text(&mut start, history.inline_text);
                    output.outbound.push(self.turn_start(&route, start));
                    return Ok(output);
                }
                let message = string(error, "message");
                if let Pending::Thread {
                    resume: Some(resume),
                    then,
                    load_key,
                } = &pending
                    && !resume.unarchived
                    && archived_session(&message)
                {
                    output.outbound.push(self.request(
                        &route,
                        "thread/unarchive",
                        json!({"threadId":resume.params["threadId"]}),
                        Pending::Unarchive {
                            params: resume.params.clone(),
                            then: then.clone(),
                            load_key: load_key.clone(),
                        },
                    ));
                    return Ok(output);
                }
                if let Pending::Terminate { process, .. } = &pending
                    && (message.contains("ProcessExited") || message.contains("InputStreamEnded"))
                {
                    if let Some(item) = self.process_item(process) {
                        output.events = self.stop_command(&item);
                    }
                    return Ok(output);
                }
                let operation = pending.operation();
                return Err(ProtocolError::Remote {
                    request: Some(id.to_string()),
                    turn_completed: completed_steer_error(&message),
                    operation,
                    message,
                });
            }
            let result = &frame["result"];
            output.replies.push(NativeReply {
                request: id.to_string(),
                operation: pending.operation(),
                result: Json(result.clone()),
            });
            match pending {
                Pending::Initialize => output.outbound.push(json!({"method":"initialized"})),
                Pending::Unarchive {
                    params,
                    then,
                    load_key,
                } => {
                    output.outbound.push(self.request(
                        &route,
                        "thread/resume",
                        params.clone(),
                        Pending::Thread {
                            resume: Some(Resume {
                                params,
                                unarchived: true,
                            }),
                            then,
                            load_key,
                        },
                    ));
                }
                Pending::Thread { then, load_key, .. } => {
                    let thread = required(&result["thread"], "id")?;
                    self.bind_root(&route, &thread, load_key);
                    output.events.push(ProviderEvent::SessionReady {
                        native_thread: thread.clone(),
                    });
                    if !std::mem::take(&mut self.route_mut(&route).stop_before_thread) {
                        match then {
                            None => {}
                            Some(Then::Turn { mut start, history }) => {
                                start["threadId"] = json!(thread);
                                output
                                    .outbound
                                    .push(self.start_or_inject(&route, start, history));
                            }
                            Some(Then::Compact) => {
                                output.outbound.push(self.compact(&route, &thread))
                            }
                        }
                    }
                }
                Pending::Inject { start, .. } => {
                    output.events.push(ProviderEvent::ContextInjected);
                    if !std::mem::take(&mut self.route_mut(&route).stop_before_thread) {
                        output.outbound.push(self.turn_start(&route, start));
                    }
                }
                Pending::Restore | Pending::Unload => {}
                Pending::RevertRead {
                    target,
                    turns,
                    mut runtime_params,
                } => {
                    if result["thread"]["historyMode"] != "paginated" {
                        return Err(ProtocolError::Invalid(format!(
                            "Cannot roll back Codex thread {}: the thread uses legacy history, which Codex 0.156 cannot revert.",
                            target.thread
                        )));
                    }
                    // `thread/revert` acts only on a thread loaded in this process.
                    if result["thread"]["status"]["type"] == "notLoaded" {
                        runtime_params["threadId"] = json!(target.thread);
                        runtime_params["excludeTurns"] = json!(true);
                        output.outbound.push(self.request(
                            &route,
                            "thread/resume",
                            runtime_params,
                            Pending::RevertResume { target, turns },
                        ));
                    } else {
                        output.outbound.push(self.revert_page(
                            &route,
                            target,
                            turns,
                            None,
                            None,
                            BTreeSet::new(),
                        )?);
                    }
                }
                Pending::RevertResume { target, turns } => {
                    output.outbound.push(self.revert_page(
                        &route,
                        target,
                        turns,
                        None,
                        None,
                        BTreeSet::new(),
                    )?);
                }
                Pending::RevertPage {
                    target,
                    mut remaining,
                    mut before,
                    visited,
                } => {
                    let turns = result["data"].as_array().ok_or_else(|| {
                        ProtocolError::Invalid("missing native turn history".into())
                    })?;
                    let mut reached = false;
                    for turn in turns {
                        let id = required(turn, "id")?;
                        if target.boundary.as_ref() == Some(&id) {
                            reached = true;
                            break;
                        }
                        before = Some(id);
                        remaining -= 1;
                        if remaining == 0 {
                            break;
                        }
                    }
                    let cursor = optional(result, "nextCursor");
                    let thread = target.thread.clone();
                    if !reached
                        && remaining > 0
                        && let Some(cursor) = cursor
                    {
                        output.outbound.push(self.revert_page(
                            &route,
                            target,
                            remaining,
                            before,
                            Some(cursor),
                            visited,
                        )?);
                    } else if let Some(before) = before {
                        output.outbound.push(self.request(
                            &route,
                            "thread/revert",
                            json!({"threadId":thread,"beforeTurnId":before}),
                            Pending::Revert {
                                thread,
                                read: false,
                            },
                        ));
                    } else {
                        output.outbound.push(self.request(
                            &route,
                            "thread/read",
                            json!({"threadId":thread,"includeTurns":false}),
                            Pending::Revert { thread, read: true },
                        ));
                    }
                }
                Pending::Revert { thread, .. } => {
                    output.completion = Some(Completion::RolledBack {
                        native_thread: thread,
                    });
                }
                Pending::Terminate { thread, process } => {
                    if result["terminated"] == false {
                        output.outbound.push(self.request(
                            &route,
                            "thread/backgroundTerminals/list",
                            json!({"threadId":thread}),
                            Pending::TerminalList { thread, process },
                        ));
                    } else if let Some(item) = self.process_item(&process) {
                        output.events = self.stop_command(&item);
                    }
                }
                Pending::TerminalList { thread, process } => {
                    let listed = result["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|terminal| terminal["processId"].as_str() == Some(process.as_str()));
                    if listed {
                        return Err(ProtocolError::Remote {
                            request: Some(id.to_string()),
                            operation: "thread/backgroundTerminals/terminate".into(),
                            message: format!(
                                "Codex background terminal {process} remained active after termination."
                            ),
                            turn_completed: false,
                        });
                    }
                    if let Some(cursor) = optional(result, "nextCursor") {
                        output.outbound.push(self.request(
                            &route,
                            "thread/backgroundTerminals/list",
                            json!({"threadId":thread,"cursor":cursor}),
                            Pending::TerminalList { thread, process },
                        ));
                    } else if let Some(item) = self.process_item(&process) {
                        output.events = self.stop_command(&item);
                    }
                }
                Pending::Operation(operation) if operation == "thread/fork" => {
                    let native_thread = required(&result["thread"], "id")?;
                    self.unclaimed.insert(native_thread.clone());
                    output.completion = Some(Completion::Forked { native_thread });
                }
                Pending::Operation(operation) => {
                    if operation == "turn/start"
                        && let Some(turn) = optional(&result["turn"], "id")
                        && let Some(thread) = self.native_thread(&route).map(str::to_owned)
                    {
                        self.turns.insert(thread.clone(), turn.clone());
                        if self.interrupt_pending.remove(&thread) {
                            output
                                .outbound
                                .extend(self.interrupt(&route, &thread, Some(&turn)));
                        }
                    }
                }
            }
            return Ok(output);
        }
        let method = required(frame, "method")?;
        let p = &frame["params"];
        let native_thread = optional(p, "threadId");
        let mut events = vec![];
        let resumes_output = matches!(
            method.as_str(),
            "item/agentMessage/delta"
                | "item/reasoning/summaryTextDelta"
                | "item/reasoning/textDelta"
                | "item/plan/delta"
                | "turn/plan/updated"
        ) || method == "item/started"
            && !matches!(
                p["item"]["type"].as_str(),
                Some("contextCompaction" | "userMessage" | "subAgentActivity")
            );
        if resumes_output
            && let Some(turn) = optional(p, "turnId")
            && let Some(recovered) = self.resolve_retry(&turn, ItemStatus::Completed)
        {
            events.push(recovered);
        }
        match method.as_str() {
            "thread/started" => {
                let thread = required(&p["thread"], "id")?;
                if !self.is_root(&thread)
                    && !self.child_roots.contains_key(&thread)
                    && !self.unclaimed.contains(&thread)
                {
                    // A new thread announced before its `thread/start` reply
                    // belongs to the only route starting one, unless a pending
                    // fork may have created it instead.
                    let mut starting =
                        self.pending
                            .values()
                            .filter_map(|(route, pending)| match pending {
                                Pending::Thread {
                                    resume: None,
                                    load_key,
                                    ..
                                } => Some(Some((route.clone(), load_key.clone()))),
                                Pending::Operation(operation) if operation == "thread/fork" => {
                                    Some(None)
                                }
                                _ => None,
                            });
                    if let (Some(Some((route, load_key))), None) =
                        (starting.next(), starting.next())
                        && self.native_thread(&route).is_none()
                    {
                        self.bind_root(&route, &thread, load_key);
                    }
                }
                if self.is_root(&thread) {
                    output.route = self.roots.get(&thread).cloned();
                    events.push(ProviderEvent::SessionReady {
                        native_thread: thread,
                    });
                }
            }
            "turn/started" => {
                let turn = required(&p["turn"], "id")?;
                if let Some(thread) = &native_thread {
                    self.turns.insert(thread.clone(), turn.clone());
                    if self.interrupt_pending.remove(thread) {
                        let route = self.route_of(Some(thread)).unwrap_or_default();
                        output
                            .outbound
                            .extend(self.interrupt(&route, thread, Some(&turn)));
                    }
                    // Each turn names the native thread it continues.
                    if self.is_root(thread) {
                        events.push(ProviderEvent::SessionReady {
                            native_thread: thread.clone(),
                        });
                    }
                }
                events.push(ProviderEvent::TurnStarted {
                    native_turn: Some(turn),
                });
            }
            "turn/completed" => {
                let turn = &p["turn"];
                let turn_id = string(turn, "id");
                let status = terminal(&string(turn, "status"));
                if let Some(thread) = &native_thread {
                    self.turns.remove(thread);
                    self.interrupt_pending.remove(thread);
                }
                self.final_answers.remove(&turn_id);
                if status == RunStatus::Failed {
                    let error = &turn["error"];
                    let recorded = self.failures.remove(&turn_id);
                    let message = optional(error, "message");
                    let code = codex_error_code(&error["codexErrorInfo"]);
                    let (message, code, class) = match recorded {
                        Some(recorded)
                            if message.as_ref().is_none_or(|m| *m == recorded.0)
                                && code.as_ref().is_none_or(|c| Some(c) == recorded.1.as_ref()) =>
                        {
                            recorded
                        }
                        _ => {
                            let class = if matches!(
                                code.as_deref(),
                                Some("usageLimitExceeded" | "rateLimitExceeded")
                            ) {
                                "usage_limit"
                            } else {
                                "provider_error"
                            };
                            (
                                message.unwrap_or_else(|| "Provider turn failed.".into()),
                                code,
                                class.into(),
                            )
                        }
                    };
                    // T3 reads the process-wide snapshot for each usage-limit
                    // failure, including a thread that attached after it arrived.
                    if class == "usage_limit" && self.rate_limits.is_some() {
                        events.push(ProviderEvent::RateLimits {
                            resets_at: usage_limit_reset(self.rate_limits.as_ref()),
                        });
                    }
                    events.push(ProviderEvent::ItemFinished {
                        key: format!("terminal-failure:{turn_id}"),
                        kind: ProviderItem::Error {
                            message,
                            retry: self.retries.remove(&turn_id),
                            code,
                            class: Some(class),
                            retryable: None,
                        },
                        text: None,
                        status: ItemStatus::Failed,
                    });
                } else if let Some(stopped) = self.resolve_retry(
                    &turn_id,
                    match status {
                        RunStatus::Completed => ItemStatus::Completed,
                        RunStatus::Cancelled => ItemStatus::Cancelled,
                        _ => ItemStatus::Interrupted,
                    },
                ) {
                    events.push(stopped);
                }
                self.failures.remove(&turn_id);
                let running = self
                    .running
                    .iter()
                    .filter(|(_, command)| command.turn == turn_id)
                    .map(|(item, command)| (item.clone(), command.command.clone()))
                    .collect::<Vec<_>>();
                if status == RunStatus::Completed && !running.is_empty() {
                    // Commands still running outlive the turn as background work.
                    self.settled.insert(turn_id.clone());
                    for (item, command) in running {
                        events.push(ProviderEvent::BackgroundTask {
                            key: item.clone(),
                            tool: item,
                            kind: BackgroundKind::Command,
                            description: command,
                            status: None,
                            summary: None,
                            exit_code: None,
                        });
                    }
                } else {
                    self.running.retain(|_, command| command.turn != turn_id);
                }
                events.push(ProviderEvent::TurnFinished {
                    status,
                    native_head: optional(turn, "id"),
                });
            }
            "item/agentMessage/delta" => {
                let key = required(p, "itemId")?;
                if !self.deferred.contains(&key) && !self.async_messages.contains(&key) {
                    events.push(ProviderEvent::TextDelta {
                        key,
                        kind: ProviderItem::Text,
                        text: string(p, "delta"),
                    });
                }
            }
            "model/rerouted" | "thread/settings/updated" => {
                let model = if method == "model/rerouted" {
                    string(p, "toModel")
                } else {
                    string(&p["threadSettings"], "model")
                };
                if !model.trim().is_empty()
                    && native_thread
                        .as_ref()
                        .is_some_and(|thread| self.children.contains_key(thread))
                {
                    events.push(ProviderEvent::ModelObserved {
                        model: model.trim().into(),
                    });
                }
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                let delta = string(p, "delta");
                if !delta.is_empty() {
                    let native = required(p, "itemId")?;
                    let (stream, index) = if method.ends_with("summaryTextDelta") {
                        ("summary", p["summaryIndex"].as_u64().unwrap_or(0))
                    } else {
                        ("content", p["contentIndex"].as_u64().unwrap_or(0))
                    };
                    let key = json!([string(p, "turnId"), native, stream, index]).to_string();
                    self.reasoning_parts
                        .entry(native)
                        .or_default()
                        .insert((stream.into(), index), key.clone());
                    events.push(ProviderEvent::TextDelta {
                        key,
                        kind: ProviderItem::Reasoning,
                        text: delta,
                    });
                }
            }
            "item/commandExecution/outputDelta" => events.push(ProviderEvent::TextDelta {
                key: required(p, "itemId")?,
                kind: ProviderItem::Command {
                    command: String::new(),
                    cwd: None,
                    exit_code: None,
                },
                text: string(p, "delta"),
            }),
            "item/plan/delta" => events.push(ProviderEvent::PlanDelta {
                key: required(p, "itemId")?,
                text: string(p, "delta"),
            }),
            "turn/plan/updated" => events.push(ProviderEvent::Plan {
                kind: PlanKind::Todo,
                key: "todo".into(),
                markdown: optional(p, "explanation").unwrap_or_default(),
                steps: p["plan"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| PlanStep {
                        text: string(s, "step"),
                        status: string(s, "status"),
                    })
                    .collect(),
            }),
            "account/rateLimits/updated" => {
                self.rate_limits = merge_rate_limits(self.rate_limits.take(), &p["rateLimits"]);
                events.push(ProviderEvent::RateLimits {
                    resets_at: usage_limit_reset(self.rate_limits.as_ref()),
                });
            }
            "thread/tokenUsage/updated" => {
                let counters = |usage: &Value| UsageCounters {
                    input: usage["inputTokens"].as_u64().unwrap_or(0),
                    cached_input: usage["cachedInputTokens"].as_u64().unwrap_or(0),
                    cache_creation: usage["cacheWriteInputTokens"].as_u64(),
                    output: usage["outputTokens"].as_u64().unwrap_or(0),
                    reasoning: usage["reasoningOutputTokens"].as_u64().unwrap_or(0),
                };
                events.push(ProviderEvent::UsageTotals {
                    native_thread: string(p, "threadId"),
                    native_turn: string(p, "turnId"),
                    total: counters(&p["tokenUsage"]["total"]),
                    last: counters(&p["tokenUsage"]["last"]),
                });
                if !self
                    .turns
                    .get(&string(p, "threadId"))
                    .is_some_and(|turn| turn != &string(p, "turnId"))
                {
                    let usage = &p["tokenUsage"]["last"];
                    events.push(ProviderEvent::ContextUsage(ContextUsage {
                        used_tokens: usage["totalTokens"].as_u64().unwrap_or(0),
                        max_tokens: p["tokenUsage"]["modelContextWindow"].as_u64(),
                        auto_compact_threshold: None,
                    }));
                    events.push(ProviderEvent::Usage(TokenUsage {
                        input: usage["inputTokens"].as_u64().unwrap_or(0),
                        cached_input: usage["cachedInputTokens"].as_u64().unwrap_or(0),
                        output: usage["outputTokens"].as_u64().unwrap_or(0),
                        reasoning_output: usage["reasoningOutputTokens"].as_u64().unwrap_or(0),
                        total: usage["totalTokens"].as_u64().unwrap_or(0),
                        max: p["tokenUsage"]["modelContextWindow"].as_u64(),
                    }));
                }
            }
            "item/started" | "item/completed" => {
                let item = &p["item"];
                let key = required(item, "id")?;
                let completed = method == "item/completed";
                let turn_id = string(p, "turnId");
                match string(item, "type").as_str() {
                    "commandExecution" => {
                        let (kind, text) = codex_item("commandExecution", item).unwrap();
                        if !completed {
                            if item_status(&string(item, "status")) == ItemStatus::Running
                                && let Some(thread) = &native_thread
                            {
                                self.running.insert(
                                    key.clone(),
                                    RunningCommand {
                                        thread: thread.clone(),
                                        turn: turn_id.clone(),
                                        command: string(item, "command"),
                                        output: string(item, "aggregatedOutput"),
                                        process: optional(item, "processId"),
                                    },
                                );
                            }
                            events.push(ProviderEvent::ItemStarted { key, kind });
                        } else {
                            let status = item_status(&string(item, "status"));
                            let retained = self.running.remove(&key);
                            events.push(ProviderEvent::ItemFinished {
                                key: key.clone(),
                                kind,
                                text: text.clone(),
                                status,
                            });
                            if let Some(retained) =
                                retained.filter(|command| self.settled.contains(&command.turn))
                            {
                                let exit_code = item["exitCode"].as_i64();
                                let detail = background_command_detail(
                                    &retained.command,
                                    exit_code,
                                    text.as_deref().unwrap_or_default(),
                                );
                                events.push(ProviderEvent::BackgroundTask {
                                    key: key.clone(),
                                    tool: key,
                                    kind: BackgroundKind::Command,
                                    description: retained.command.clone(),
                                    status: Some(match exit_code {
                                        Some(0) => ItemStatus::Completed,
                                        Some(_) => ItemStatus::Failed,
                                        None => ItemStatus::Pending,
                                    }),
                                    summary: Some(detail.clone()),
                                    exit_code,
                                });
                                if !self.stopping.contains(&retained.thread) {
                                    events.push(ProviderEvent::Wake {
                                        text: detail,
                                        detail: Some(retained.command),
                                    });
                                }
                                self.release_settled(&retained.turn);
                            }
                        }
                    }
                    "agentMessage" => {
                        let asynchronous = item["delivery"] == "async";
                        let questions = item["questions"].as_array().filter(|q| !q.is_empty());
                        if asynchronous {
                            self.async_messages.insert(key.clone());
                            if completed && let Some(questions) = questions {
                                events.push(ProviderEvent::RequestOpened {
                                    owner_path: vec![],
                                    key: format!("async:{key}"),
                                    body: RequestBody::Questions {
                                        questions: async_questions(questions),
                                    },
                                    capability: ResponseCapability::Message,
                                });
                            }
                        } else {
                            let final_answer = item["phase"] != "commentary";
                            if !completed {
                                if final_answer {
                                    let answers =
                                        self.final_answers.entry(turn_id.clone()).or_default();
                                    let first = answers.first.get_or_insert_with(|| key.clone());
                                    if *first != key || !answers.texts.is_empty() {
                                        self.deferred.insert(key.clone());
                                    }
                                }
                                if !self.deferred.contains(&key) {
                                    events.push(ProviderEvent::ItemStarted {
                                        key,
                                        kind: ProviderItem::Text,
                                    });
                                }
                            } else {
                                self.deferred.remove(&key);
                                let text = string(item, "text");
                                let answers =
                                    self.final_answers.entry(turn_id.clone()).or_default();
                                let repeated = final_answer
                                    && !answers.texts.is_empty()
                                    && (text.is_empty() || answers.texts.contains(&text));
                                if !repeated {
                                    if final_answer {
                                        answers.texts.insert(text.clone());
                                    }
                                    events.push(ProviderEvent::ItemFinished {
                                        key,
                                        kind: ProviderItem::Text,
                                        text: Some(text),
                                        status: ItemStatus::Completed,
                                    });
                                }
                            }
                        }
                    }
                    "dynamicToolCall" | "mcpToolCall" => {
                        let kind = codex_tool(item);
                        events.push(if completed {
                            ProviderEvent::ItemFinished {
                                key,
                                kind,
                                text: None,
                                status: item_status(&string(item, "status")),
                            }
                        } else {
                            ProviderEvent::ItemStarted { key, kind }
                        });
                    }
                    "reasoning" => {
                        if completed {
                            let mut parts = self.reasoning_parts.remove(&key).unwrap_or_default();
                            for stream in ["summary", "content"] {
                                for (index, text) in
                                    item[stream].as_array().into_iter().flatten().enumerate()
                                {
                                    if text.as_str().is_some_and(|text| !text.is_empty()) {
                                        parts.entry((stream.into(), index as u64)).or_insert_with(
                                            || {
                                                json!([string(p, "turnId"), key, stream, index])
                                                    .to_string()
                                            },
                                        );
                                    }
                                }
                            }
                            for ((stream, index), key) in parts {
                                events.push(ProviderEvent::ItemFinished {
                                    key,
                                    kind: ProviderItem::Reasoning,
                                    text: item[stream][index as usize]
                                        .as_str()
                                        .filter(|text| !text.is_empty())
                                        .map(str::to_owned),
                                    status: ItemStatus::Completed,
                                });
                            }
                        }
                    }

                    "userMessage" => {
                        if completed {
                            events.push(ProviderEvent::UserMessage {
                                key,
                                text: item["content"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|b| b["text"].as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            });
                        }
                    }
                    "subAgentActivity" => {
                        let child = required(item, "agentThreadId")?;
                        if string(item, "kind") == "started" {
                            self.adopt_child(&child, native_thread.as_ref());
                        } else if !self.children.contains_key(&child) {
                            return Ok(output);
                        }
                        match string(item, "kind").as_str() {
                            "started" => events.push(ProviderEvent::SubagentStarted {
                                background: false,
                                native_thread: Some(child.clone()),
                                key: child,
                                parent: native_thread
                                    .clone()
                                    .filter(|id| self.children.contains_key(id)),
                                prompt: String::new(),
                                model: optional(item, "model"),
                            }),
                            "completed" | "closed" | "interrupted" => {
                                events.push(ProviderEvent::SubagentFinished {
                                    key: child,
                                    status: ItemStatus::Completed,
                                    result: string(item, "message"),
                                })
                            }
                            _ => {}
                        }
                        events.push(ProviderEvent::SubagentNamed {
                            key: required(item, "agentThreadId")?,
                            title: string(item, "agentPath"),
                        });
                    }
                    "collabAgentToolCall" => {
                        let tool = string(item, "tool");
                        let receivers = item["receiverThreadIds"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        for child in receivers {
                            // Only a spawn assigns a parent; other calls address known children.
                            if tool == "spawnAgent" {
                                self.adopt_child(&child, native_thread.as_ref());
                            } else if !self.children.contains_key(&child) {
                                continue;
                            }
                            if matches!(tool.as_str(), "spawnAgent" | "sendInput" | "resumeAgent") {
                                events.push(ProviderEvent::SubagentStarted {
                                    background: false,
                                    native_thread: Some(child.clone()),
                                    key: child.clone(),
                                    parent: native_thread
                                        .clone()
                                        .filter(|id| self.children.contains_key(id)),
                                    prompt: string(item, "prompt"),
                                    model: optional(item, "model"),
                                });
                            }
                            let state = &item["agentsStates"][&child];
                            if completed
                                && let Some(status) =
                                    state["status"].as_str().and_then(collab_terminal_status)
                            {
                                events.push(ProviderEvent::SubagentFinished {
                                    key: child,
                                    status,
                                    result: string(state, "message"),
                                });
                            }
                        }
                    }
                    "plan" => events.push(ProviderEvent::Plan {
                        kind: PlanKind::Proposed,
                        key,
                        markdown: string(item, "text"),
                        steps: vec![],
                    }),
                    kind => {
                        if let Some((kind, text)) = codex_item(kind, item) {
                            events.push(if completed {
                                ProviderEvent::ItemFinished {
                                    key,
                                    kind,
                                    text,
                                    status: item_status(&string(item, "status")),
                                }
                            } else {
                                ProviderEvent::ItemStarted { key, kind }
                            });
                        }
                    }
                }
            }
            "item/tool/requestUserInput" => {
                self.server_requests
                    .insert(frame["id"].to_string(), method.clone());
                events.push(ProviderEvent::RequestOpened {
                    owner_path: vec![],
                    key: frame["id"].to_string(),
                    body: RequestBody::Questions {
                        questions: questions(&p["questions"]),
                    },
                    capability: ResponseCapability::Live,
                })
            }
            "serverRequest/resolved" => {
                self.server_requests.remove(&p["requestId"].to_string());
                events.push(ProviderEvent::RequestClosed {
                    key: p["requestId"].to_string(),
                })
            }
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "mcpServer/elicitation/request" => {
                if method == "mcpServer/elicitation/request"
                    && (mcp_elicitation_response(p, ApprovalDecision::Accept)["action"] != "accept"
                        || !p["turnId"]
                            .as_str()
                            .is_some_and(|turn| self.turns.values().any(|active| active == turn)))
                {
                    output
                        .outbound
                        .push(json!({"id":frame["id"],"result":{"action":"decline"}}));
                    return Ok(output);
                }
                let kind = if method.contains("fileChange")
                    || p["permissions"]["fileSystem"]["write"]
                        .as_array()
                        .is_some_and(|paths| !paths.is_empty())
                {
                    "file-change"
                } else if p["permissions"]["fileSystem"]["read"]
                    .as_array()
                    .is_some_and(|paths| !paths.is_empty())
                {
                    "file-read"
                } else if method.starts_with("mcp") {
                    "mcp-elicitation"
                } else {
                    "command"
                };
                let (title, options) = if method.starts_with("mcp") {
                    describe_mcp_elicitation(p)
                } else {
                    (
                        optional(p, "command").unwrap_or_else(|| kind.into()),
                        vec![
                            ApprovalOption {
                                decision: ApprovalDecision::Accept,
                                label: "Approve".into(),
                            },
                            ApprovalOption {
                                decision: ApprovalDecision::AcceptForSession,
                                label: "Approve for session".into(),
                            },
                            ApprovalOption {
                                decision: ApprovalDecision::Decline,
                                label: "Decline".into(),
                            },
                            ApprovalOption {
                                decision: ApprovalDecision::Cancel,
                                label: "Cancel".into(),
                            },
                        ],
                    )
                };
                events.push(ProviderEvent::RequestOpened {
                    owner_path: vec![],
                    key: frame["id"].to_string(),
                    body: RequestBody::Approval {
                        kind: kind.into(),
                        title,
                        detail: optional(p, "reason"),
                        options,
                        input: Json(p.clone()),
                    },
                    capability: ResponseCapability::Live,
                });
            }
            "error" => {
                let error = &p["error"];
                let turn = string(p, "turnId");
                let message = optional(error, "additionalDetails")
                    .map(|details| details.trim().to_owned())
                    .filter(|details| !details.is_empty())
                    .unwrap_or_else(|| string(error, "message"));
                let code = codex_error_code(&error["codexErrorInfo"]);
                let class = match code.as_deref() {
                    Some("usageLimitExceeded" | "rateLimitExceeded") => "usage_limit",
                    Some(code)
                        if code.starts_with("http") || code.starts_with("responseStream") =>
                    {
                        "transport_error"
                    }
                    _ => "provider_error",
                };
                self.failures
                    .insert(turn.clone(), (message.clone(), code.clone(), class.into()));
                if p["willRetry"] == true {
                    let previous = self.retries.get(&turn).cloned();
                    let progress = retry_progress(&string(error, "message"));
                    let retry = RetryProgress {
                        attempt: progress
                            .map(|(attempt, _)| attempt)
                            .unwrap_or_else(|| previous.as_ref().map_or(1, |r| r.attempt + 1)),
                        max_attempts: progress
                            .and_then(|(_, max)| max)
                            .or(previous.and_then(|r| r.max_attempts)),
                        delay_ms: None,
                    };
                    self.retries.insert(turn.clone(), retry.clone());
                    events.push(ProviderEvent::ItemStarted {
                        key: format!("terminal-failure:{turn}"),
                        kind: ProviderItem::Error {
                            message,
                            retry: Some(retry),
                            code,
                            class: Some(class.into()),
                            retryable: Some(true),
                        },
                    });
                }
            }
            // Account, process and diagnostic notifications do not alter a conversation.
            _ => {}
        }
        if method == "item/completed"
            && p["item"]["type"] == "contextCompaction"
            && let Some(thread) = &native_thread
            && let Some(restore) = self.restore_additional_context(thread)
        {
            output.outbound.push(restore);
        }
        if output.route.is_none() {
            output.route = self.route_of(native_thread.as_deref());
        }
        if let Some(thread) = native_thread.filter(|t| !self.is_root(t)) {
            let path = native_path(&thread, &self.children)?;
            let mut routed = vec![];
            for mut event in events {
                match &mut event {
                    ProviderEvent::RequestOpened { owner_path, .. } => {
                        *owner_path = path.iter().rev().map(|s| (*s).to_owned()).collect();
                        output.events.push(event);
                    }
                    ProviderEvent::RequestClosed { .. } => output.events.push(event),
                    _ => routed.push(event),
                }
            }
            output
                .events
                .extend(child_events(routed, &thread, &self.children)?);
        } else {
            output.events = events;
        }
        Ok(output)
    }
}
fn replace_input_text(start: &mut Value, text: String) {
    let inputs = start["input"]
        .as_array_mut()
        .expect("turn input is an array");
    inputs.retain(|input| input["type"] != "text");
    inputs.insert(0, json!({"type":"text","text":text}));
}
/// Text first, then native images as data URLs, as the reference adapter sends them.
fn codex_input(
    text: &str,
    attachments: &[Attachment],
    images: &[PreparedImage],
) -> Result<Vec<Value>, ProtocolError> {
    let mut input = vec![];
    let text = attachment_text(&codex_skill_mention_text(text), attachments);
    if !text.is_empty() {
        input.push(json!({"type":"text","text":text}));
    }
    for file in attachments.iter().filter(|file| native_image(file)) {
        let image = images
            .iter()
            .find(|image| image.attachment_id == file.id)
            .ok_or_else(|| ProtocolError::Invalid(format!("missing prepared image {}", file.id)))?;
        input.push(json!({"type":"image","url":format!("data:{};base64,{}", image.mime_type, image.base64)}));
    }
    if input.is_empty() {
        return Err(ProtocolError::Invalid(
            "Turn requires non-empty text or attachments.".into(),
        ));
    }
    Ok(input)
}
fn codex_runtime(mode: RuntimeMode) -> (&'static str, &'static str, &'static str) {
    match mode {
        RuntimeMode::ApprovalRequired => ("untrusted", "user", "readOnly"),
        RuntimeMode::AutoAcceptEdits => ("on-request", "user", "workspaceWrite"),
        RuntimeMode::Auto => ("on-request", "auto_review", "workspaceWrite"),
        RuntimeMode::FullAccess => ("never", "user", "dangerFullAccess"),
    }
}
/// T3 mergeCodexRateLimits: a model-specific snapshot never replaces the main
/// one, and fields an update omits keep their earlier value.
fn merge_rate_limits(previous: Option<Value>, update: &Value) -> Option<Value> {
    if update["limitId"]
        .as_str()
        .is_some_and(|limit| !limit.is_empty() && limit != "codex")
    {
        return previous;
    }
    let Some(mut merged) = previous else {
        return Some(update.clone());
    };
    for key in [
        "limitId",
        "planType",
        "rateLimitReachedType",
        "primary",
        "secondary",
    ] {
        if let Some(value) = update.get(key) {
            merged[key] = value.clone();
        }
    }
    Some(merged)
}
/// T3 codexUsageLimitResetAt: the latest reset of the exhausted windows, when
/// each of them reports one (Unix seconds).
fn usage_limit_reset(snapshot: Option<&Value>) -> Option<i64> {
    let snapshot = snapshot?;
    if snapshot["limitId"]
        .as_str()
        .is_some_and(|limit| !limit.is_empty() && limit != "codex")
    {
        return None;
    }
    let exhausted = [&snapshot["primary"], &snapshot["secondary"]]
        .into_iter()
        .filter(|window| {
            window["usedPercent"]
                .as_f64()
                .is_some_and(|used| used.is_finite() && used >= 100.0)
        })
        .map(|window| {
            window["resetsAt"]
                .as_f64()
                .filter(|at| at.is_finite() && *at > 0.0)
        })
        .collect::<Option<Vec<_>>>()?;
    exhausted.into_iter().map(|at| at as i64).max()
}
fn codex_error_code(info: &Value) -> Option<String> {
    match info {
        Value::String(code) => Some(code.clone()),
        Value::Object(map) => map.keys().next().cloned(),
        _ => None,
    }
}
/// `N/M` progress in a retry message.
fn retry_progress(message: &str) -> Option<(u64, Option<u64>)> {
    let bytes = message.as_bytes();
    let slash = (0..bytes.len()).find(|&i| {
        bytes[i] == b'/'
            && message[..i]
                .trim_end()
                .ends_with(|c: char| c.is_ascii_digit())
            && message[i + 1..]
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit())
    })?;
    let left = message[..slash].trim_end();
    let start = left
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let right = message[slash + 1..].trim_start();
    let end = right
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(right.len());
    Some((left[start..].parse().ok()?, right[..end].parse().ok()))
}
fn collab_terminal_status(status: &str) -> Option<ItemStatus> {
    Some(match status {
        "completed" => ItemStatus::Completed,
        "interrupted" => ItemStatus::Interrupted,
        "errored" | "notFound" => ItemStatus::Failed,
        "shutdown" => ItemStatus::Cancelled,
        _ => return None,
    })
}
fn async_questions(questions: &[Value]) -> Vec<Question> {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let nonblank = |value: Option<&str>, fallback: &str| {
                value
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(fallback)
                    .to_owned()
            };
            Question {
                required: true,
                id: index.to_string(),
                header: "Question".into(),
                question: nonblank(question["title"].as_str(), "Choose an answer."),
                multiple: false,
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|label| QuestionOption {
                        label: label.into(),
                        description: Some(label.into()),
                    })
                    .collect(),
            }
        })
        .collect()
}
fn background_command_detail(command: &str, exit_code: Option<i64>, output: &str) -> String {
    let command = if command.chars().count() > 200 {
        format!("{}...", command.chars().take(200).collect::<String>())
    } else {
        command.into()
    };
    let exit = exit_code.map_or(String::new(), |code| format!(" (exit {code})"));
    let output = output.trim_end();
    let tail = if output.chars().count() > 1000 {
        let skip = output.chars().count() - 1000;
        format!("...{}", output.chars().skip(skip).collect::<String>())
    } else {
        output.into()
    };
    let header = format!("Background command completed{exit}: {command}");
    if tail.is_empty() {
        header
    } else {
        format!("{header}\n\nOutput tail:\n{tail}")
    }
}
fn codex_item(kind: &str, item: &Value) -> Option<(ProviderItem, Option<String>)> {
    Some(match kind {
        "agentMessage" => (ProviderItem::Text, optional(item, "text")),
        "commandExecution" => (
            ProviderItem::Command {
                command: string(item, "command"),
                cwd: optional(item, "cwd"),
                exit_code: item["exitCode"].as_i64(),
            },
            optional(item, "aggregatedOutput"),
        ),
        "fileChange" => (
            ProviderItem::FileChange {
                changes: Json(item["changes"].clone()),
            },
            None,
        ),
        "webSearch" => (
            ProviderItem::WebSearch {
                query: optional(item, "query")
                    .or_else(|| optional(&item["action"], "query"))
                    .unwrap_or_default(),
                results: item.get("results").cloned().map(Json),
            },
            None,
        ),
        "dynamicToolCall" | "mcpToolCall" => (codex_tool(item), None),
        "contextCompaction" => (
            ProviderItem::Compaction {
                before: item["beforeTokenCount"].as_u64(),
                after: item["afterTokenCount"].as_u64(),
            },
            None,
        ),
        _ => return None,
    })
}
#[cfg(test)]
impl CodexProtocol {
    pub(crate) fn replay_outbound(&mut self, frame: &Value) {
        if frame["method"] == "turn/start"
            && let Some(thread) = optional(&frame["params"], "threadId")
        {
            self.bind_root("", &thread, None);
        }
        if let Some(id) = frame["id"].as_u64() {
            let method = string(frame, "method");
            let pending = match method.as_str() {
                "initialize" => Pending::Initialize,
                "thread/start" | "thread/resume" => Pending::Thread {
                    resume: (method == "thread/resume").then(|| Resume {
                        params: frame["params"].clone(),
                        unarchived: false,
                    }),
                    then: None,
                    load_key: None,
                },
                _ => Pending::Operation(method),
            };
            self.next_id = self.next_id.max(id);
            self.pending.insert(id, (String::new(), pending));
        }
    }
}
/// T3's `/\bsession \S+ is archived\b|\bcodex unarchive\b/i`.
fn archived_session(message: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let message = message.to_lowercase();
    let bounded = |at: usize, len: usize| {
        !message[..at].ends_with(word) && !message[at + len..].starts_with(word)
    };
    if message
        .match_indices("codex unarchive")
        .any(|(at, found)| bounded(at, found.len()))
    {
        return true;
    }
    message.match_indices("session ").any(|(at, _)| {
        if message[..at].ends_with(word) {
            return false;
        }
        let rest = &message[at + "session ".len()..];
        let id = rest.len() - rest.trim_start_matches(|c: char| !c.is_whitespace()).len();
        id > 0
            && rest[id..]
                .strip_prefix(" is archived")
                .is_some_and(|tail| !tail.starts_with(word))
    })
}
pub fn completed_steer_error(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "turn completed",
        "no active turn",
        "cannot steer a compact turn",
        "expected active turn id",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}
pub fn native_image_mime(mime: &str) -> bool {
    matches!(
        mime.to_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
}

// T3 codexUsageLimits.test.ts.
#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    #[test]
    fn keeps_windows_an_update_does_not_carry() {
        let merged = merge_rate_limits(
            Some(
                json!({"limitId":"codex","planType":"business","primary":{"usedPercent":100,"resetsAt":1_800_000_000,"windowDurationMins":300}}),
            ),
            &json!({"rateLimitReachedType":"rate_limit_reached"}),
        );
        assert_eq!(
            merged,
            Some(
                json!({"limitId":"codex","planType":"business","rateLimitReachedType":"rate_limit_reached","primary":{"usedPercent":100,"resetsAt":1_800_000_000,"windowDurationMins":300}})
            )
        );
    }

    #[test]
    fn ignores_a_model_specific_snapshot_so_it_cannot_replace_the_main_allowance() {
        let main = json!({"limitId":"codex","primary":{"usedPercent":100,"resetsAt":1_800_000_000,"windowDurationMins":300}});
        assert_eq!(
            merge_rate_limits(
                Some(main.clone()),
                &json!({"limitId":"spark","primary":{"usedPercent":3,"resetsAt":1_800_000_000,"windowDurationMins":300}}),
            ),
            Some(main)
        );
    }

    #[test]
    fn waits_for_every_exhausted_window_and_never_invents_an_unknown_reset() {
        let reset = |snapshot: Value| usage_limit_reset(Some(&snapshot));
        assert_eq!(
            reset(
                json!({"primary":{"usedPercent":100,"resetsAt":2000000000},"secondary":{"usedPercent":100,"resetsAt":2000100000}})
            ),
            Some(2_000_100_000)
        );
        assert_eq!(reset(json!({"primary":{"usedPercent":100}})), None);
        assert_eq!(
            reset(json!({"primary":{"usedPercent":50,"resetsAt":2000000000}})),
            None
        );
    }

    #[test]
    fn rate_limit_updates_report_the_merged_reset() {
        let mut codex = CodexProtocol::default();
        let update = |rate_limits: Value| json!({"method":"account/rateLimits/updated","params":{"rateLimits":rate_limits}});
        codex
            .receive(&update(
                json!({"limitId":"codex","primary":{"usedPercent":100,"resetsAt":2000000000}}),
            ))
            .unwrap();
        let output = codex
            .receive(&update(
                json!({"rateLimitReachedType":"rate_limit_reached"}),
            ))
            .unwrap();
        assert_eq!(
            output.events,
            vec![ProviderEvent::RateLimits {
                resets_at: Some(2_000_000_000)
            }]
        );
    }
}
