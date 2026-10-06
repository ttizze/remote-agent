//! Domain records for sync tests.
use agent_domain::*;
use agent_protocol::conversation::{SequencedFact, SnapshotWindow, ThreadSnapshot, ThreadUpdate};
use std::{collections::BTreeMap, sync::Arc};

pub fn at() -> Timestamp {
    Timestamp::parse("2026-06-20T00:00:00Z").unwrap()
}
pub fn thread_id() -> ThreadId {
    ThreadId::new("thread").unwrap()
}
pub fn selection(instance: &str) -> ModelSelection {
    ModelSelection {
        instance: instance.into(),
        driver: if instance == "claude" {
            Driver::Claude
        } else {
            Driver::Codex
        },
        model: "gpt".into(),
        options: BTreeMap::new(),
    }
}
pub fn thread_state(title: &str) -> State {
    let step = ThreadMachine::step(
        &State::default(),
        &InputEnvelope {
            at: at(),
            key: "create".into(),
            input: Input::Command {
                id: CommandId::new("create").unwrap(),
                command: Box::new(Command::Create {
                    thread: thread_id(),
                    project: "project".into(),
                    title: title.into(),
                    selection: selection("codex"),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                    workspace: None,
                    created_by: MessageAuthor::User,
                    creation_source: "desktop".into(),
                }),
                receipt: None,
            },
        },
    );
    fold(&State::default(), &step.facts).unwrap()
}
pub fn fact(body: FactBody) -> Fact {
    Fact { at: at(), body }
}
pub fn renamed(title: &str) -> Fact {
    fact(FactBody::ThreadRenamed {
        title: title.into(),
    })
}
pub fn projected(item: Item) -> Fact {
    fact(FactBody::ItemProjected { item })
}
pub fn sequenced(sequence: u64, fact: Fact) -> SequencedFact {
    SequencedFact {
        sequence,
        thread_sequence: sequence,
        fact,
    }
}
pub fn facts(items: Vec<(u64, Fact)>) -> ThreadUpdate {
    ThreadUpdate::Facts(
        items
            .into_iter()
            .map(|(sequence, fact)| sequenced(sequence, fact))
            .collect(),
    )
}
pub fn title_update(title: &str, sequence: u64) -> ThreadUpdate {
    facts(vec![(sequence, renamed(title))])
}
pub fn snapshot(state: State, sequence: u64, window: Option<SnapshotWindow>) -> ThreadUpdate {
    ThreadUpdate::Snapshot(ThreadSnapshot {
        snapshot_sequence: sequence,
        thread_sequence: sequence,
        state: Arc::new(state),
        window,
    })
}
pub fn window(cursor: Option<&str>, has_more: bool, latest: Option<u64>) -> SnapshotWindow {
    SnapshotWindow {
        history_cursor: cursor.map(Into::into),
        has_more_history: has_more,
        latest_local_ordinal: latest,
        payload_budget_exceeded: false,
    }
}
pub fn command_item(id: &str, ordinal: u64) -> Item {
    Item {
        id: TurnItemId::new(id).unwrap(),
        run: None,
        attempt: None,
        native_key: id.into(),
        ordinal,
        kind: ItemKind::CommandExecution {
            command: "pwd".into(),
            cwd: None,
            exit_code: Some(0),
            title: None,
        },
        status: ItemStatus::Completed,
        text: String::new(),
        started_at: at(),
        completed_at: Some(at()),
        output_omitted: false,
        output_indicates_failure: false,
    }
}
pub fn run(id: &str, ordinal: u64, status: RunStatus) -> Run {
    Run {
        restart_of: None,
        restart_cancelled_work: vec![],
        checkpoint_scope: None,
        native_baseline_heads: BTreeMap::new(),
        id: RunId::new(id).unwrap(),
        ordinal,
        message: MessageId::new(format!("message-{id}")).unwrap(),
        selection: selection("codex"),
        status,
        attempt: None,
        queue_position: None,
        queue_held: false,
        requested_at: at(),
        started_at: Some(at()),
        completed_at: None,
        source_plan: None,
        checkpoint: None,
        continuation: false,
    }
}
pub fn plan(id: &str, run: &str) -> Plan {
    Plan {
        kind: PlanKind::Proposed,
        id: PlanId::new(id).unwrap(),
        run: RunId::new(run).unwrap(),
        native_key: id.into(),
        markdown: String::new(),
        steps: vec![],
        implemented_by: None,
    }
}
pub fn title(state: &State) -> &str {
    &state.thread.as_ref().unwrap().title
}
