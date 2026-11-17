//! Model traces exercise the production writer/outbox on private file databases.
//! Run `scripts/dev-env.sh just lean-delivery` to generate the required corpus.
//! This checks the durable delivery boundary, not external provider execution.
use super::*;
use agent_protocol::orchestration as model;
use serde::Deserialize;
use std::{cell::Cell, fs::File, io::BufRead};

const COMMAND: &str = "delivery-input";
const THREAD: &str = "delivery-thread";
const KIND: &str = "message.dispatch";
const NOW: &str = test_support::NOW;
const LATER: &str = "2026-11-15T00:00:00.000Z";

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Mode {
    Native,
    ReplaySafe,
}

#[derive(Debug, Deserialize)]
struct Action {
    name: String,
    worker: u64,
    accepted: bool,
    commit: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Job {
    phase: String,
    worker: Option<u64>,
    attempts: u64,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    receipt: Option<bool>,
    input: Job,
    cleanup: Job,
    evaluations: usize,
    prepared: bool,
    uncertain: bool,
    lane_open: bool,
}

#[derive(Debug, Deserialize)]
struct Step {
    operation: Action,
    ok: bool,
    state: Snapshot,
}

#[derive(Deserialize)]
struct Trace {
    mode: Mode,
    steps: Vec<Step>,
}

struct Harness {
    // Close SQLite before the temporary directory is removed (also on Windows).
    storage: Storage,
    directory: tempfile::TempDir,
    mode: Mode,
    evaluations: Cell<usize>,
    original_receipt: Option<commands::Receipt>,
}

impl Harness {
    fn new(mode: Mode) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join(CONVERSATION_DATABASE_FILE));
        storage
            .with(|db| {
                let tx = db.transaction().map_err(failure)?;
                projections::save(&tx, None, &test_support::thread(THREAD))?;
                tx.commit().map_err(failure)
            })
            .unwrap();
        Self {
            directory,
            storage,
            mode,
            evaluations: Cell::new(0),
            original_receipt: None,
        }
    }

    fn effects(&self) -> [Option<model::Effect>; 2] {
        self.storage
            .with(|db| {
                // The runtime's working set omits settled rows. Conformance
                // also checks that those durable rows survive reopen/replay.
                let mut query = db
                    .prepare(&format!(
                        "SELECT {} FROM orchestration_v2_effect_outbox WHERE thread_id=?1",
                        event_store::EFFECT_COLUMNS
                    ))
                    .map_err(failure)?;
                let mut rows = query.query([THREAD]).map_err(failure)?;
                let mut effects = [None, None];
                while let Some(row) = rows.next().map_err(failure)? {
                    let effect = event_store::effect(row)?;
                    let index = match effect.request {
                        model::EffectRequest::ProviderTurnStart { .. }
                        | model::EffectRequest::CheckpointCapture { .. } => 0,
                        model::EffectRequest::ProviderSessionDetach { .. } => 1,
                        _ => panic!("unexpected delivery reservation"),
                    };
                    assert!(
                        effects[index].replace(effect).is_none(),
                        "duplicate reservation"
                    );
                }
                Ok(effects)
            })
            .unwrap()
    }

    fn submit(&self, accepted: bool, commit: bool) -> bool {
        let prior_receipt = self.storage.thread_receipt(COMMAND, THREAD, KIND).unwrap();
        let prior_head = self.storage.with(|db| event_store::head(db, None)).unwrap();
        let prior_state = self.storage.thread_state(THREAD, &[]).unwrap();
        if !commit {
            self.storage.with(|db| db.execute_batch("CREATE TEMP TRIGGER abort_delivery_receipt BEFORE INSERT ON orchestration_command_receipts BEGIN SELECT RAISE(ABORT,'injected commit failure'); END").map_err(failure)).unwrap();
        }
        let result = self.storage.thread_command(
            commands::Command {
                id: COMMAND,
                thread_id: THREAD,
                kind: KIND,
                now: NOW,
                item_ids: &[],
            },
            None,
            |state| {
                self.evaluations.set(self.evaluations.get() + 1);
                if !accepted {
                    return Err("input rejected".into());
                }
                match self.mode {
                    Mode::Native => model::admit(
                        state,
                        model::Input {
                            message_id: "input".into(),
                            created_by: model::Actor::User,
                            creation_source: model::CreationSource::Mobile,
                            scheduled_task_id: None,
                            text: "delivery contract".into(),
                            attachments: vec![],
                            context: None,
                            model_selection: state.thread.model_selection.clone(),
                            driver: "codex".into(),
                            workspace_preparation: None,
                            fork_origin: None,
                            fresh_context: false,
                        },
                        model::Admission::Queue,
                        model::Context {
                            command_id: COMMAND,
                            now: NOW,
                            next_run_ordinal: state.next_run_ordinal().unwrap(),
                            can_steer: false,
                        },
                    ),
                    Mode::ReplaySafe => Ok(model::Decision {
                        changes: vec![],
                        effects: vec![checkpoint("delivery-checkpoint")],
                    }),
                }
            },
        );
        if !commit {
            self.storage
                .with(|db| {
                    db.execute_batch("DROP TRIGGER abort_delivery_receipt")
                        .map_err(failure)
                })
                .unwrap();
        }
        if prior_receipt.is_some() || result.is_err() {
            assert_eq!(
                self.storage.thread_state(THREAD, &[]).unwrap(),
                prior_state,
                "replay/failed commit changed the projection"
            );
            assert_eq!(
                self.storage.with(|db| event_store::head(db, None)).unwrap(),
                prior_head,
                "replay/failed commit appended events"
            );
        }
        match result {
            Ok(executed) => {
                if let Some(prior) = prior_receipt {
                    assert_eq!(executed.receipt, prior);
                    assert!(
                        executed.events.is_empty(),
                        "receipt replay published events"
                    );
                }
                assert_eq!(
                    Some(executed.receipt.clone()),
                    self.storage.thread_receipt(COMMAND, THREAD, KIND).unwrap()
                );
                executed.receipt.error.is_none()
            }
            Err(error) => {
                assert!(!commit, "unexpected admission failure: {error}");
                false
            }
        }
    }

    fn apply(&mut self, action: &Action, index: usize) -> bool {
        let worker = format!("worker-{}", action.worker);
        match action.name.as_str() {
            "submit" => self.submit(action.accepted, action.commit),
            "register" => {
                let [Some(saved), _] = self.effects() else {
                    return false;
                };
                self.storage
                    .with(|db| {
                        let tx = db.transaction().map_err(failure)?;
                        event_store::save_effect(
                            &tx,
                            &model::Effect {
                                status: model::EffectStatus::Pending,
                                attempt_count: 0,
                                lease_owner: None,
                                lease_expires_at: None,
                                completed_at: None,
                                last_error: None,
                                updated_at: LATER.into(),
                                ..saved.clone()
                            },
                        )?;
                        tx.commit().map_err(failure)
                    })
                    .unwrap();
                assert_eq!(
                    self.effects()[0],
                    Some(saved),
                    "registration reset durable work"
                );
                true
            }
            "claim" => self.storage.claim_effect(&worker, LATER).unwrap().is_some(),
            "prepare" => self.prepare(&worker),
            "acknowledge" | "notSent" | "acknowledgeCleanup" | "retryCleanup" => {
                let [input, cleanup] = self.effects();
                let selected = if action.name.ends_with("Cleanup") {
                    cleanup
                } else {
                    input
                };
                let Some(effect) = selected else {
                    return false;
                };
                let settlement = if action.name.starts_with("acknowledge") {
                    outbox::Settlement::Succeeded
                } else {
                    outbox::Settlement::Retry {
                        available_at: LATER,
                        error: "confirmed NotSent",
                    }
                };
                self.storage
                    .finish_effect(&effect.id, Some(&worker), LATER, settlement)
                    .unwrap()
            }
            "loseReply" => {
                let [Some(effect), _] = self.effects() else {
                    return false;
                };
                self.storage
                    .observe_effect(&effect, &worker, LATER, |state| {
                        model::uncertain_delivery(state, &effect.id, "reply lost", LATER)
                    })
                    .is_ok_and(|events| !events.is_empty())
            }
            "resolve" => {
                let id = format!("resolve-{index}");
                let items = [model::user_item_id(THREAD, "input")];
                self.storage
                    .thread_command(
                        commands::Command {
                            id: &id,
                            thread_id: THREAD,
                            kind: "message.resolve-unknown",
                            now: LATER,
                            item_ids: &items,
                        },
                        None,
                        |state| {
                            model::cancel_uncertain(
                                state,
                                "input",
                                model::Context {
                                    command_id: &id,
                                    now: LATER,
                                    next_run_ordinal: state.next_run_ordinal().unwrap(),
                                    can_steer: false,
                                },
                            )
                        },
                    )
                    .unwrap()
                    .receipt
                    .error
                    .is_none()
            }
            "reopen" | "restart" => {
                self.storage =
                    Storage::open(&self.directory.path().join(CONVERSATION_DATABASE_FILE));
                self.storage.check_ready().unwrap();
                if action.name == "restart" {
                    self.storage.recover_effects(LATER).unwrap();
                    self.storage.runtime_restarted(false, LATER).unwrap();
                }
                true
            }
            name => panic!("unknown model operation: {name}"),
        }
    }

    fn prepare(&self, worker: &str) -> bool {
        let [Some(effect), _] = self.effects() else {
            return false;
        };
        self.storage
            .observe_effect(&effect, worker, LATER, |state| {
                let model::EffectRequest::ProviderTurnStart { run_id } = &effect.request else {
                    return Err("preparation requires a native start".into());
                };
                let run = state.runs.get(run_id).ok_or("input run missing")?;
                let id = run
                    .provider_thread_id
                    .as_deref()
                    .ok_or("provider thread missing")?;
                let expected = state
                    .provider_threads
                    .get(id)
                    .and_then(|thread| thread.native_thread_ref.as_ref());
                model::native_resource_loaded(
                    state,
                    id,
                    expected,
                    model::ProviderSession {
                        id: "delivery-session".into(),
                        driver: "codex".into(),
                        provider_instance_id: "codex".into(),
                        status: model::ProviderSessionStatus::Ready,
                        cwd: "/workspace".into(),
                        model: None,
                        capabilities: Default::default(),
                        created_at: LATER.into(),
                        updated_at: LATER.into(),
                        last_error: None,
                    },
                    model::ProviderRef {
                        driver: "codex".into(),
                        native_id: Some("delivery-native".into()),
                        strength: model::NativeRefStrength::Strong,
                        fingerprint: None,
                        ordinal: None,
                    },
                    false,
                    LATER,
                )
            })
            .is_ok_and(|events| !events.is_empty())
    }

    fn lane_open(&self) -> bool {
        self.storage
            .with(|db| {
                let tx = db.transaction().map_err(failure)?;
                event_store::save_effect(&tx, &checkpoint("zz-delivery-probe"))?;
                let claimed = outbox::claim(&tx, "lane-probe", LATER)?;
                let open = claimed.is_some_and(|effect| effect.id == "zz-delivery-probe");
                tx.rollback().map_err(failure)?;
                Ok(open)
            })
            .unwrap()
    }

    fn snapshot(&mut self) -> Snapshot {
        let receipt = self.storage.thread_receipt(COMMAND, THREAD, KIND).unwrap();
        if self.original_receipt.is_some() {
            assert_eq!(receipt, self.original_receipt, "a durable receipt changed");
        } else {
            self.original_receipt = receipt.clone();
        }
        let accepted = receipt.as_ref().is_some_and(|value| value.error.is_none());
        self.check_admission_rows(accepted);
        let [input, cleanup] = self.effects();
        assert_eq!(input.is_some(), accepted);
        let state = self.storage.thread_state(THREAD, &[]).unwrap();
        Snapshot {
            receipt: receipt.map(|value| value.error.is_none()),
            input: job(input.as_ref()),
            cleanup: job(cleanup.as_ref()),
            evaluations: self.evaluations.get(),
            prepared: state
                .attached_provider_sessions
                .contains("delivery-session"),
            uncertain: state.has_uncertain_input(),
            lane_open: self.lane_open(),
        }
    }

    fn check_admission_rows(&self, accepted: bool) {
        let count = i64::from(accepted && matches!(self.mode, Mode::Native));
        self.storage
            .with(|db| {
                for (table, predicate) in [
                    ("orchestration_v2_projection_runs", "1"),
                    ("orchestration_v2_projection_run_attempts", "1"),
                    ("orchestration_v2_projection_provider_threads", "1"),
                    ("orchestration_v2_projection_nodes", "1"),
                    (
                        "orchestration_v2_projection_turn_items",
                        "type='user_message'",
                    ),
                ] {
                    let actual: i64 = db
                        .query_row(
                            &format!(
                                "SELECT count(*) FROM {table} WHERE thread_id=?1 AND {predicate}"
                            ),
                            [THREAD],
                            |row| row.get(0),
                        )
                        .map_err(failure)?;
                    assert_eq!(actual, count, "partial or duplicate admission in {table}");
                }
                Ok(())
            })
            .unwrap();
    }
}

fn job(effect: Option<&model::Effect>) -> Job {
    let (phase, worker) = match effect {
        None => ("absent", None),
        Some(effect) => match effect.status {
            model::EffectStatus::Pending => ("pending", None),
            model::EffectStatus::Running => match effect.lease_owner.as_deref() {
                Some(worker) => (
                    "running",
                    Some(worker.strip_prefix("worker-").unwrap().parse().unwrap()),
                ),
                None => ("unknown", None),
            },
            model::EffectStatus::Succeeded => ("succeeded", None),
            model::EffectStatus::Cancelled => ("cancelled", None),
            status => panic!("unexpected delivery phase: {status:?}"),
        },
    };
    Job {
        phase: phase.into(),
        worker,
        attempts: effect.map_or(0, |value| value.attempt_count),
    }
}

fn checkpoint(id: &str) -> model::Effect {
    model::Effect {
        id: id.into(),
        command_id: COMMAND.into(),
        thread_id: THREAD.into(),
        request: model::EffectRequest::CheckpointCapture {
            run_id: "probe".into(),
            scope_id: "probe".into(),
        },
        status: model::EffectStatus::Pending,
        attempt_count: 0,
        available_at: NOW.into(),
        lease_owner: None,
        lease_expires_at: None,
        created_at: NOW.into(),
        updated_at: NOW.into(),
        completed_at: None,
        last_error: None,
    }
}

#[test]
#[ignore = "requires checked Lean traces; run scripts/dev-env.sh just lean-delivery"]
fn sqlite_matches_lean_delivery_traces() {
    let path = std::env::var("AGENT_DELIVERY_TRACES").expect("Lean trace path is required");
    let mut traces = 0;
    let mut operations = 0;
    for line in std::io::BufReader::new(File::open(path).unwrap()).lines() {
        let trace: Trace = serde_json::from_str(&line.unwrap()).unwrap();
        let mut harness = Harness::new(trace.mode);
        for (index, step) in trace.steps.into_iter().enumerate() {
            let actual = harness.apply(&step.operation, index);
            assert_eq!(
                actual, step.ok,
                "trace {traces}, step {index}: {:?}",
                step.operation
            );
            assert_eq!(
                harness.snapshot(),
                step.state,
                "trace {traces}, step {index}: {:?}",
                step.operation
            );
            operations += 1;
        }
        traces += 1;
    }
    assert!(traces > 0, "Lean generated no traces");
    println!("Lean/SQLite conformance: {traces} traces, {operations} operations matched");
}
