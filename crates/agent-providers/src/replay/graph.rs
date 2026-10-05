use super::*;

/// Executes cross-thread domain commands at the native boundaries recorded by
/// the reference transcript; no actor or persistence implementation is needed.
struct GraphReplay {
    replay: Replay,
    roots: BTreeMap<String, ThreadId>,
    requests: BTreeMap<u64, (ThreadId, String)>,
    forks: BTreeMap<u64, (ThreadId, CommandId, ThreadId)>,
    claude_fork: Option<(ThreadId, CommandId, ThreadId)>,
    fork_order: Vec<ThreadId>,
    merges: usize,
}
impl GraphReplay {
    fn new(driver: Driver, rows: &[Value]) -> Self {
        Self {
            replay: Replay::new(driver, rows),
            roots: BTreeMap::new(),
            requests: BTreeMap::new(),
            forks: BTreeMap::new(),
            claude_fork: None,
            fork_order: vec![],
            merges: 0,
        }
    }
    fn select(&mut self, thread: &ThreadId) {
        self.replay.root = thread.clone();
        self.replay.owner = self
            .replay
            .state()
            .active_run()
            .and_then(|r| r.attempt.clone())
            .or_else(|| {
                self.replay
                    .state()
                    .runs
                    .last()
                    .and_then(|r| r.attempt.clone())
            });
    }
    fn root_for(&mut self, native: &str) -> ThreadId {
        self.roots
            .entry(native.into())
            .or_insert_with(|| ThreadId::new("root").unwrap())
            .clone()
    }
    fn capture(&mut self) {
        let ready: Vec<_> = self
            .replay
            .states
            .iter()
            .flat_map(|(thread, state)| {
                state
                    .runs
                    .iter()
                    .filter(|run| {
                        run.status.terminal()
                            && run.status != RunStatus::RolledBack
                            && run.checkpoint.is_none()
                    })
                    .map(|run| (thread.clone(), run.clone()))
            })
            .collect();
        for (thread, run) in ready {
            let baselines = if self.replay.states[&thread]
                .checkpoints
                .iter()
                .any(|c| c.run_ordinal == 0)
            {
                vec![]
            } else {
                vec![CapturedBaseline {
                    status: CheckpointStatus::Ready,
                    checkpoint: CheckpointId::new(format!("{thread}-baseline")).unwrap(),
                    ordinal: 0,
                    file_ref: "baseline".into(),
                    native_heads: run.native_baseline_heads.clone(),
                }]
            };
            let reply = self.replay.apply(
                &thread,
                Input::Effect(EffectResult::CheckpointCaptured {
                    status: CheckpointStatus::Ready,
                    baselines,
                    run: run.id.clone(),
                    attempt: run.attempt,
                    checkpoint: CheckpointId::new(format!("{thread}-checkpoint-{}", run.ordinal))
                        .unwrap(),
                    file_ref: format!("checkpoint-{}", run.ordinal),
                }),
            );
            assert_eq!(reply, Reply::Accepted);
        }
    }
    fn fork(&mut self, parent: ThreadId, head: Option<&str>) -> (ThreadId, CommandId, ThreadId) {
        self.capture();
        self.select(&parent);
        let state = self.replay.state();
        let through = state
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
            .clone();
        let target = ThreadId::new(format!("fork-{}", self.fork_order.len())).unwrap();
        let reply = self.replay.command(
            &parent,
            Command::Fork {
                target: target.clone(),
                through_run: through,
                title: Some("Forked thread".into()),
            },
        );
        assert_eq!(reply, Reply::Thread(target.clone()));
        let (_, command, provider) = self.replay.native_forks.last().unwrap();
        let command = command.clone();
        assert!(
            matches!(provider,ProviderCommand::Fork{through_turn,..} if head.is_none_or(|head|through_turn.as_deref()==Some(head)))
        );
        // The translator must keep the native cursor, rather than choosing an
        // application turn count or the latest cursor of a different actor.
        if self.replay.driver == Driver::Claude {
            assert!(
                matches!(ClaudeProtocol::default().command(provider,"",&[]).unwrap().process,Some(ProcessDirective::Fork{through_head,..}) if head.is_none_or(|head|through_head.as_deref()==Some(head)))
            );
        } else {
            let wire = CodexProtocol::default()
                .command(provider, &wire_context(), &[])
                .unwrap();
            assert_eq!(wire[0]["method"], "thread/fork");
            if let Some(head) = head {
                assert_eq!(wire[0]["params"]["lastTurnId"], head);
            }
        }
        self.fork_order.push(target.clone());
        (parent, command, target)
    }
    fn forked(&mut self, pending: (ThreadId, CommandId, ThreadId), native: String) {
        let (parent, command, target) = pending;
        assert_eq!(
            self.replay.apply(
                &parent,
                Input::Effect(EffectResult::NativeForked {
                    command,
                    native_thread: native.clone()
                })
            ),
            Reply::Accepted
        );
        self.roots.insert(native, target.clone());
    }
    fn rollback_to(&mut self, thread: &ThreadId, head: Option<&str>) {
        self.capture();
        let state = &self.replay.states[thread];
        let cp = state
            .checkpoints
            .iter()
            .find(|cp| {
                cp.native_heads
                    .get(&state.thread.as_ref().unwrap().selection.instance)
                    .and_then(Option::as_deref)
                    == head
            })
            .unwrap()
            .id
            .clone();
        self.select(thread);
        assert_eq!(
            self.replay.command(
                thread,
                Command::Rollback {
                    checkpoint: cp,
                    restore_files: false
                }
            ),
            Reply::Accepted
        );
        let (_, provider) = self.replay.provider_commands.last().unwrap();
        assert!(
            matches!(provider,ProviderCommand::Rollback{absolute_head,..} if absolute_head.as_deref()==head)
        );
        if self.replay.driver == Driver::Claude {
            assert!(
                matches!(ClaudeProtocol::default().command(provider,"",&[]).unwrap().process,Some(ProcessDirective::Resume{absolute_head,..}) if absolute_head.as_deref()==head)
            );
        }
    }
    fn finish_rollback(&mut self, thread: &ThreadId) {
        let command = self.replay.states[thread]
            .rollback
            .as_ref()
            .unwrap()
            .command
            .clone();
        assert_eq!(
            self.replay.apply(
                thread,
                Input::Effect(EffectResult::RollbackFinished {
                    bindings: vec![],
                    command
                })
            ),
            Reply::Accepted
        );
    }
    fn outbound(&mut self, frame: &Value) {
        let method = string(frame, "method");
        let kind = string(frame, "type");
        if method == "thread/fork" {
            let parent = self.root_for(&string(&frame["params"], "threadId"));
            let pending = self.fork(parent, frame["params"]["lastTurnId"].as_str());
            self.forks.insert(frame["id"].as_u64().unwrap(), pending);
        } else if kind == "session.fork" {
            let head = frame["options"]["upToMessageId"].as_str();
            let parent = self
                .replay
                .states
                .iter()
                .find(|(_, state)| {
                    state
                        .attempts
                        .iter()
                        .any(|a| head.is_some() && a.native_head.as_deref() == head)
                })
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| ThreadId::new("root").unwrap());
            self.claude_fork = Some(self.fork(parent, head));
        } else if kind == "query.open" {
            let native = frame["options"]["resume"]
                .as_str()
                .or_else(|| frame["options"]["sessionId"].as_str())
                .unwrap();
            let thread = self.root_for(native);
            self.select(&thread);
            if let Some(head) = frame["options"]["resumeSessionAt"].as_str() {
                self.rollback_to(&thread, Some(head));
                self.finish_rollback(&thread);
            }
        } else if method == "thread/revert" {
            let thread = self.root_for(&string(&frame["params"], "threadId"));
            self.capture();
            let discarded = frame["params"]["beforeTurnId"].as_str().unwrap();
            let state = &self.replay.states[&thread];
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
            let ordinal = run.ordinal;
            let head = state
                .checkpoints
                .iter()
                .find(|c| c.run_ordinal == ordinal - 1)
                .unwrap()
                .native_heads
                .get(&state.thread.as_ref().unwrap().selection.instance)
                .cloned()
                .flatten();
            self.rollback_to(&thread, head.as_deref());
        } else if method == "turn/start" || kind == "prompt.offer" {
            let thread = if self.replay.driver == Driver::Codex {
                self.root_for(&string(&frame["params"], "threadId"))
            } else {
                self.replay.root.clone()
            };
            self.select(&thread);
            let text = if self.replay.driver == Driver::Claude {
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
            let text = if let Some((_, user)) = text.split_once("User message:\n") {
                let source = self.fork_order[self.merges].clone();
                self.merges += 1;
                assert_eq!(
                    self.replay.command(
                        &source,
                        Command::MergeBack {
                            target: thread.clone()
                        }
                    ),
                    Reply::Accepted
                );
                user.to_owned()
            } else {
                text
            };
            self.replay.send(text, false);
            if self.replay.driver == Driver::Codex {
                let owner = self.replay.owner.clone().unwrap();
                self.replay.apply(
                    &thread,
                    Input::Provider {
                        attempt: owner,
                        event: Box::new(ProviderEvent::SessionReady {
                            native_thread: string(&frame["params"], "threadId"),
                        }),
                    },
                );
            }
        } else if method == "turn/interrupt" || kind == "query.interrupt" {
            let thread = self.replay.root.clone();
            self.replay.command(&thread, Command::Stop);
        }
        if self.replay.driver == Driver::Codex {
            if let Some(id) = frame["id"].as_u64() {
                let thread = frame["params"]["threadId"]
                    .as_str()
                    .map(|native| self.root_for(native))
                    .unwrap_or_else(|| self.replay.root.clone());
                self.requests.insert(id, (thread, method));
            }
            self.replay.codex.replay_outbound(frame);
        }
    }
    fn inbound(&mut self, frame: &Value) {
        if frame["type"] == "session.forked" {
            let pending = self.claude_fork.take().unwrap();
            self.forked(pending, string(frame, "sessionId"));
            return;
        }
        if let Some(id) = frame["id"].as_u64() {
            if let Some(pending) = self.forks.remove(&id) {
                self.forked(pending, string(&frame["result"]["thread"], "id"));
            }
            if let Some((thread, method)) = self.requests.remove(&id) {
                self.select(&thread);
                if method == "thread/revert" {
                    self.finish_rollback(&thread);
                }
                if (method == "thread/start" || method == "thread/resume")
                    && let Some(native) = frame["result"]["thread"]["id"].as_str()
                {
                    self.roots.insert(native.into(), thread);
                }
            }
        } else if let Some(thread) = frame["params"]["threadId"]
            .as_str()
            .and_then(|id| self.roots.get(id))
            .cloned()
        {
            self.select(&thread);
        }
        self.replay.receive(frame);
        self.capture();
    }
    fn run(scenario: &str, driver: Driver) -> Self {
        let rows = transcript(scenario, driver);
        let mut graph = Self::new(driver, &rows);
        for row in &rows {
            match row["type"].as_str() {
                Some("expect_outbound") => graph.outbound(&row["frame"]),
                Some("emit_inbound") => graph.inbound(&row["frame"]),
                _ => {}
            }
        }
        graph.replay.integrity();
        graph
    }
}
fn wire_context() -> WireContext {
    WireContext {
        cwd: "<workspace>".into(),
        client_name: "agent-client".into(),
        client_version: "test".into(),
        ..WireContext::default()
    }
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

#[test]
fn native_fork_replays_preserve_the_selected_boundary_and_keep_sibling_deltas_separate() {
    for driver in [Driver::Codex, Driver::Claude] {
        for scenario in [
            "thread_fork_native",
            "thread_fork_native_prior_turn",
            "thread_fork_native_continue",
            "thread_fork_native_siblings",
        ] {
            let graph = GraphReplay::run(scenario, driver);
            let root = ThreadId::new("root").unwrap();
            assert_eq!(
                graph.replay.states.len(),
                if scenario.ends_with("siblings") { 3 } else { 2 },
                "{scenario} {driver:?}"
            );
            let source = &graph.replay.states[&root];
            for target in &graph.fork_order {
                let child = &graph.replay.states[target];
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
                let child = &graph.replay.states[&graph.fork_order[0]];
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

#[test]
fn rollback_replays_hide_discarded_local_items_and_preserve_the_native_boundary() {
    for (scenario, driver) in [
        ("thread_rollback", Driver::Codex),
        ("thread_rollback", Driver::Claude),
        ("thread_rollback_after_restart", Driver::Codex),
        ("thread_rollback_to_stopped_turn", Driver::Codex),
        ("thread_fork_native_fork_local_rollback", Driver::Claude),
    ] {
        let graph = GraphReplay::run(scenario, driver);
        let state = if scenario.contains("fork_local") {
            &graph.replay.states[&graph.fork_order[0]]
        } else {
            &graph.replay.states[&ThreadId::new("root").unwrap()]
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

#[test]
fn merge_back_replays_deliver_only_each_fork_delta_and_preserve_source_conversation() {
    for driver in [Driver::Codex, Driver::Claude] {
        for scenario in ["thread_merge_back_continue", "thread_merge_back_siblings"] {
            let graph = GraphReplay::run(scenario, driver);
            let source = &graph.replay.states[&ThreadId::new("root").unwrap()];
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
                for (i, child) in graph.fork_order.iter().enumerate() {
                    let text = visible_text(&graph.replay.states[child]);
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
        }
    }
}

#[test]
fn delegated_task_status_replay_keeps_the_original_result_while_followups_run_and_queue() {
    let rows = transcript("delegated_task_status", Driver::Codex);
    let mut graph = GraphReplay::new(Driver::Codex, &rows);
    let parent = ThreadId::new("root").unwrap();
    let child = ThreadId::new("delegated-child").unwrap();
    let task = NodeId::new("delegated-task").unwrap();
    let mut starts = 0;
    let mut turns = 0;
    let mut original = None;
    let mut original_transfer = None;
    let observe = |graph: &GraphReplay| {
        let owner = &graph.replay.states[&parent];
        let child = &graph.replay.states[&child];
        delegated_task_status(
            &owner.tasks[0],
            &child.runs,
            &child.items,
            &owner.transfers,
            &child.messages,
        )
    };
    for row in &rows {
        let frame = &row["frame"];
        if row["type"] == "expect_outbound" {
            if frame["method"] == "thread/start" {
                starts += 1;
                graph.outbound(frame);
                if starts == 2 {
                    let selection = graph.replay.states[&parent]
                        .thread
                        .as_ref()
                        .unwrap()
                        .selection
                        .clone();
                    assert_eq!(
                        graph.replay.command(
                            &parent,
                            Command::Delegate {
                                task: task.clone(),
                                child: child.clone(),
                                prompt: "Inspect the delegated API boundary and return the result."
                                    .into(),
                                selection,
                                wake: CompletionWake::SettledOnly
                            }
                        ),
                        Reply::Thread(child.clone())
                    );
                    assert_eq!(
                        graph.replay.states[&child]
                            .thread
                            .as_ref()
                            .unwrap()
                            .parent
                            .as_ref(),
                        Some(&parent)
                    );
                    assert!(graph.replay.states[&child].native_owner.is_none());
                    graph.requests.insert(
                        frame["id"].as_u64().unwrap(),
                        (child.clone(), "thread/start".into()),
                    );
                    graph.select(&child);
                }
            } else if frame["method"] == "turn/start" && starts == 2 {
                turns += 1;
                if turns == 2 {
                    graph.outbound(frame);
                } else {
                    graph.select(&child);
                    graph.replay.codex.replay_outbound(frame);
                    let attempt = graph
                        .replay
                        .state()
                        .active_run()
                        .unwrap()
                        .attempt
                        .clone()
                        .unwrap();
                    graph.replay.owner = Some(attempt.clone());
                    graph.replay.apply(
                        &child,
                        Input::Provider {
                            attempt,
                            event: Box::new(ProviderEvent::SessionReady {
                                native_thread: string(&frame["params"], "threadId"),
                            }),
                        },
                    );
                }
            } else if frame["method"] == "turn/interrupt" {
                let active = graph.replay.states[&child].active_run().unwrap().id.clone();
                assert_eq!(
                    graph.replay.command(
                        &child,
                        Command::Interrupt {
                            run: active,
                            hold_queue: false
                        }
                    ),
                    Reply::Accepted
                );
                graph.replay.codex.replay_outbound(frame);
            } else {
                graph.outbound(frame);
            }
        } else if row["type"] == "emit_inbound" {
            graph.inbound(frame);
            if frame["method"] == "turn/completed" && turns == 1 {
                let status = observe(&graph);
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
                graph.select(&child);
                graph.replay.send(
                    "Complete the queued follow-up and return the final result.".into(),
                    false,
                );
                let state = &graph.replay.states[&child];
                assert_eq!(
                    state.runs.iter().map(|run| run.status).collect::<Vec<_>>(),
                    [RunStatus::Completed, RunStatus::Running, RunStatus::Queued]
                );
                assert_eq!(
                    graph.replay.states[&parent].active_run().unwrap().status,
                    RunStatus::Running
                );
                let status = observe(&graph);
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
    }
    let status = observe(&graph);
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
        Some(graph.replay.states[&child].runs[2].id.clone())
    );
    assert_eq!(status.latest_terminal_status, Some(RunStatus::Completed));
    assert_eq!(
        status.latest_terminal_summary.as_deref(),
        Some("Queued delegated follow-up completed.")
    );
    assert_eq!(status.latest_terminal_result_context_transfer_id, None);
    assert_eq!(
        graph.replay.states[&child].runs[1].status,
        RunStatus::Interrupted
    );
    graph.replay.integrity();
}
