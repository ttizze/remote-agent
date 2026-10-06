//! A bounded client snapshot followed by later client facts folds without error and
//! agrees with the client form of the full fold for everything it holds and
//! everything in its window.
use super::*;
use crate::StoredFact;
use crate::sync::{client_facts, client_state};
use agent_domain::{
    Command, DispatchMode, Input, InputEnvelope, ProviderEvent, ProviderItem, SendMessage,
    ThreadMachine, fold,
};
use proptest::prelude::*;
use std::sync::Arc;

#[derive(Debug, Clone)]
enum Op {
    Send { queue: bool },
    Steer,
    Item(u8),
    Delta(u8),
    FinishItem,
    Plan,
    Finish,
    Stop,
    Rename,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => any::<bool>().prop_map(|queue| Op::Send { queue }),
        1 => Just(Op::Steer),
        3 => (0u8..4).prop_map(Op::Item),
        3 => (1u8..40).prop_map(Op::Delta),
        2 => Just(Op::FinishItem),
        1 => Just(Op::Plan),
        2 => Just(Op::Finish),
        1 => Just(Op::Stop),
        1 => Just(Op::Rename),
    ]
}

struct Script {
    state: State,
    facts: Vec<Fact>,
    /// Snapshots are taken between steps, after a whole commit.
    commits: Vec<usize>,
    /// The state after each commit, which projects that commit's facts.
    states: Vec<State>,
    step: usize,
    open: Vec<(String, ProviderItem)>,
}

impl Script {
    fn new() -> Self {
        let mut script = Self {
            state: State::default(),
            facts: vec![],
            commits: vec![],
            states: vec![],
            step: 0,
            open: vec![],
        };
        script.command(Command::Create {
            workspace: None,
            thread: thread_id(),
            project: "project-1".into(),
            title: "Thread".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
        });
        script
    }
    fn input(&mut self, input: Input) {
        self.step += 1;
        let envelope = InputEnvelope {
            at: at(),
            key: format!("{}#{}", thread_id(), self.step),
            input,
        };
        let step = ThreadMachine::step(&self.state, &envelope);
        for fact in &step.facts {
            apply(&mut self.state, fact).unwrap();
        }
        self.facts.extend(step.facts);
        self.commits.push(self.facts.len());
        self.states.push(self.state.clone());
    }
    fn command(&mut self, command: Command) {
        let id = agent_domain::CommandId::new(format!("command-{}", self.step)).unwrap();
        self.input(Input::Command {
            id,
            command: Box::new(command),
            receipt: None,
        });
    }
    fn provider(&mut self, event: ProviderEvent) {
        let Some(attempt) = self.state.active_run().and_then(|run| run.attempt.clone()) else {
            return;
        };
        self.input(Input::Provider {
            attempt,
            event: Box::new(event),
        });
    }
    fn send(&mut self, mode: DispatchMode) {
        let id = MessageId::new(format!("message-{}", self.step)).unwrap();
        self.command(Command::Send(SendMessage {
            title_seed: None,
            created_by: MessageAuthor::User,
            creation_source: "client".into(),
            id,
            text: format!("prompt {}", self.step),
            attachments: vec![],
            selection: None,
            mode,
            intent: None,
            source_plan: None,
            resolved_plan: None,
            continuation: None,
        }));
    }
    /// A started run gets its native session and turn, like a live provider.
    fn settle(&mut self) {
        let Some(attempt) = self.state.active_run().and_then(|run| run.attempt.clone()) else {
            return;
        };
        if self
            .state
            .attempts
            .iter()
            .any(|a| a.id == attempt && a.native_turn.is_none())
        {
            self.open.clear();
            self.provider(ProviderEvent::SessionReady {
                native_thread: "native-thread".into(),
            });
            self.provider(ProviderEvent::TurnStarted {
                native_turn: Some(format!("turn-{}", self.step)),
            });
        }
    }
    fn run(&mut self, op: Op) {
        let active = self.state.active_run().map(|run| run.id.clone());
        match op {
            Op::Send { queue } => self.send(if queue && active.is_some() {
                DispatchMode::QueueAfterActive
            } else {
                DispatchMode::StartImmediately
            }),
            Op::Steer => {
                if let Some(run) = active {
                    self.send(DispatchMode::SteerActive { run });
                }
            }
            Op::Item(kind) => {
                let key = format!("key-{}", self.step);
                let kind = match kind {
                    0 => ProviderItem::Text,
                    1 => ProviderItem::Reasoning,
                    2 => ProviderItem::Tool {
                        presentation: Default::default(),
                        name: "tool".into(),
                        input: agent_domain::Json(serde_json::json!({ "q": "x".repeat(20_000) })),
                        output: Some(agent_domain::Json(serde_json::json!({ "threadId": "t" }))),
                    },
                    _ => ProviderItem::Command {
                        command: "ls".into(),
                        cwd: None,
                        exit_code: None,
                    },
                };
                self.open.push((key.clone(), kind.clone()));
                self.provider(ProviderEvent::ItemStarted { key, kind });
            }
            Op::Delta(length) => {
                if let Some((key, kind)) = self.open.last().cloned() {
                    self.provider(ProviderEvent::TextDelta {
                        key,
                        kind,
                        text: "d".repeat(length as usize),
                    });
                }
            }
            Op::FinishItem => {
                if let Some((key, kind)) = self.open.pop() {
                    self.provider(ProviderEvent::ItemFinished {
                        key,
                        kind,
                        text: None,
                        status: ItemStatus::Completed,
                    });
                }
            }
            Op::Plan => self.provider(ProviderEvent::Plan {
                kind: PlanKind::Proposed,
                key: format!("plan-{}", self.step),
                markdown: format!("plan {}", self.step),
                steps: vec![],
            }),
            Op::Finish => self.provider(ProviderEvent::TurnFinished {
                status: RunStatus::Completed,
                native_head: Some(format!("head-{}", self.step)),
            }),
            Op::Stop => {
                if let Some(run) = active {
                    self.command(Command::Interrupt {
                        run,
                        hold_queue: false,
                        reason: None,
                    });
                    self.provider(ProviderEvent::TurnFinished {
                        status: RunStatus::Interrupted,
                        native_head: None,
                    });
                }
            }
            Op::Rename => self.command(Command::Rename {
                title: format!("Title {}", self.step),
            }),
        }
        self.settle();
    }
}

fn message_ids(state: &State) -> HashSet<&MessageId> {
    state.items.iter().filter_map(message_of).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn bounded_snapshot_then_later_facts_matches_the_full_fold_within_the_window(
        ops in proptest::collection::vec(op(), 1..60),
        split in 0.0f64..=1.0,
        turns in proptest::option::of(1usize..3),
        max_items in 1usize..6,
    ) {
        let mut script = Script::new();
        for op in ops {
            script.run(op);
        }
        let facts = script.facts;
        let index = ((script.commits.len() - 1) as f64 * split) as usize;
        let cut = script.commits[index];
        let prefix = Arc::new(fold(&State::default(), &facts[..cut]).unwrap());
        let policy = PagePolicy {
            max_user_turns: turns,
            max_items,
            max_encoded_bytes: 10_000_000,
            max_frame_bytes: HISTORY_FRAME_MAX_BYTES,
        };
        let client = client_state(&prefix);
        let bounded = bounded_state(&client, cut as u64, policy);
        let window_start = bounded.history_cursor.as_deref().map_or(0, |cursor| {
            let cursor = HistoryCursor::decode(cursor).unwrap();
            prefix.items.iter().find(|item| item.id.as_str() == cursor.item).unwrap().ordinal
        });

        let mut after = Ok(bounded.state);
        for (commit, end) in script.commits.iter().enumerate().skip(index + 1) {
            let start = script.commits[commit - 1];
            let stored: Arc<[StoredFact]> = facts[start..*end]
                .iter()
                .enumerate()
                .map(|(offset, fact)| StoredFact {
                    global_seq: (start + offset + 1) as u64,
                    thread_seq: (start + offset + 1) as u64,
                    fact: fact.clone(),
                })
                .collect();
            let delivered: Vec<Fact> = client_facts(&script.states[commit], &stored)
                .iter()
                .map(|stored| stored.fact.clone())
                .collect();
            after = after.and_then(|state| fold(&state, &delivered));
        }
        prop_assert!(after.is_ok(), "{:?}", after.err());
        let after = after.unwrap();
        let full = State::clone(&client_state(&Arc::new(script.state)));

        prop_assert_eq!(&after.thread, &full.thread);
        prop_assert_eq!(&after.runs, &full.runs);
        prop_assert_eq!(&after.attempts, &full.attempts);
        prop_assert_eq!(&after.requests, &full.requests);
        prop_assert_eq!(&after.checkpoints, &full.checkpoints);
        prop_assert_eq!(after.plans.len(), full.plans.len());
        for item in &after.items {
            prop_assert_eq!(Some(item), full.items.iter().find(|full| full.id == item.id));
        }
        for item in full.items.iter().filter(|item| item.ordinal >= window_start) {
            prop_assert!(after.items.contains(item), "{} is missing", item.id);
        }
        for message in &after.messages {
            prop_assert_eq!(Some(message), full.messages.iter().find(|full| full.id == message.id));
        }
        let held: HashSet<_> = after.messages.iter().map(|message| &message.id).collect();
        prop_assert!(message_ids(&after).is_subset(&held));
    }
}
