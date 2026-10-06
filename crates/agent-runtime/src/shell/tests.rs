use super::*;
use crate::store::tests::{at, selection};
use agent_domain::{
    Command, CommandId, DispatchMode, Input, InputEnvelope, InteractionMode, MessageAuthor,
    ProviderEvent, ProviderItem, RecoveryTrigger, Reply, RunAttemptId, RunStatus, RuntimeMode,
    SendMessage, Step, ThreadId, ThreadMachine, apply,
};

fn step(state: &mut State, key: &str, input: Input) -> Step {
    let step = ThreadMachine::step(
        state,
        &InputEnvelope {
            at: at(),
            key: key.into(),
            input,
        },
    );
    for fact in &step.facts {
        apply(state, fact).unwrap();
    }
    step
}
fn command(state: &mut State, key: &str, command: Command) -> Step {
    step(
        state,
        key,
        Input::Command {
            id: CommandId::new(key).unwrap(),
            command: Box::new(command),
            receipt: None,
        },
    )
}
fn provider(state: &mut State, key: &str, attempt: &RunAttemptId, event: ProviderEvent) -> Step {
    step(
        state,
        key,
        Input::Provider {
            attempt: attempt.clone(),
            event: Box::new(event),
        },
    )
}
fn created() -> State {
    let mut state = State::default();
    command(
        &mut state,
        "create",
        Command::Create {
            thread: ThreadId::new("thread:shell").unwrap(),
            project: "project".into(),
            title: "Thread".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: None,
        },
    );
    state
}
fn send(key: &str) -> Command {
    Command::Send(SendMessage {
        created_by: MessageAuthor::User,
        creation_source: "client".into(),
        id: MessageId::new(key).unwrap(),
        text: key.into(),
        attachments: vec![],
        selection: None,
        mode: DispatchMode::StartImmediately,
        intent: None,
        source_plan: None,
        title_seed: None,
    })
}
fn running(state: &mut State, key: &str) -> RunAttemptId {
    let Reply::Run(run) = command(state, key, send(key)).reply else {
        panic!("the message starts a run")
    };
    let attempt = state
        .runs
        .iter()
        .find(|r| r.id == run)
        .unwrap()
        .attempt
        .clone()
        .unwrap();
    provider(
        state,
        &format!("{key}:session"),
        &attempt,
        ProviderEvent::SessionReady {
            native_thread: "native-thread".into(),
        },
    );
    provider(
        state,
        &format!("{key}:started"),
        &attempt,
        ProviderEvent::TurnStarted {
            native_turn: Some(key.into()),
        },
    );
    attempt
}
fn finish(state: &mut State, key: &str, attempt: &RunAttemptId) {
    provider(
        state,
        key,
        attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            native_head: Some("native-head".into()),
        },
    );
}
/// Whether a startup `Recover` records anything beyond its unconditional marker.
fn recover(state: &mut State) -> bool {
    let step = step(
        state,
        "recover",
        Input::Recover {
            trigger: RecoveryTrigger::Startup,
            continue_after_restart: false,
            capturing: state.captures.keys().cloned().collect(),
        },
    );
    step.facts
        .iter()
        .any(|fact| !matches!(fact.body, FactBody::BackgroundWorkStopped))
        || !step.effects.is_empty()
}
/// The persisted flag must select exactly the threads `Recover` changes.
fn assert_flag_matches_recovery(mut state: State, expected: bool) {
    assert_eq!(needs_recovery(&state), expected);
    assert_eq!(recover(&mut state), expected);
    assert!(!needs_recovery(&state));
}

#[test]
fn flags_only_threads_whose_recovery_changes_something() {
    assert_flag_matches_recovery(created(), false);

    let mut state = created();
    running(&mut state, "running");
    assert_flag_matches_recovery(state, true);

    let mut state = created();
    let attempt = running(&mut state, "settled");
    finish(&mut state, "settled:finish", &attempt);
    assert_eq!(state.runs[0].status, RunStatus::Completed);
    assert_flag_matches_recovery(state, false);
}

// T3 ProviderRuntimeRecoveryService.ts recovers background items on settled runs.
#[test]
fn flags_background_work_that_outlives_a_completed_run() {
    let mut state = created();
    let attempt = running(&mut state, "root");
    provider(
        &mut state,
        "child",
        &attempt,
        ProviderEvent::SubagentStarted {
            background: true,
            native_thread: None,
            key: "child".into(),
            parent: None,
            prompt: "Background subagent".into(),
            model: None,
        },
    );
    finish(&mut state, "root:finish", &attempt);
    assert_eq!(state.runs[0].status, RunStatus::Completed);
    assert!(!state.runs.iter().any(|run| run.status.blocking()));
    assert!(state.captures.is_empty() && state.rollback.is_none());
    assert_flag_matches_recovery(state, true);
}

#[test]
fn flags_a_native_child_without_runs_of_its_own() {
    let mut child = created();
    let owner = RunAttemptId::new("native-owner").unwrap();
    command(
        &mut child,
        "bind",
        Command::BindNativeChild {
            native_thread: Some("native-child".into()),
            owner: owner.clone(),
            parent: ThreadId::new("thread:parent").unwrap(),
            task: NodeId::new("task").unwrap(),
            generation: 0,
        },
    );
    provider(
        &mut child,
        "output",
        &owner,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: "partial".into(),
        },
    );
    assert!(child.runs.is_empty() && child.captures.is_empty());
    assert_flag_matches_recovery(child, true);
}

#[test]
fn indexes_an_assistant_answer_when_its_item_completes() {
    let mut state = created();
    let attempt = running(&mut state, "question");
    let delta = provider(
        &mut state,
        "answer",
        &attempt,
        ProviderEvent::TextDelta {
            key: "answer".into(),
            kind: ProviderItem::Text,
            text: "the searchable answer".into(),
        },
    );
    let streaming = search_changes(&state, &delta.facts);
    assert!(
        !streaming
            .upserts
            .iter()
            .any(|row| row.text == "the searchable answer")
    );

    let finished = provider(
        &mut state,
        "finish",
        &attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            native_head: None,
        },
    );
    assert!(
        !finished
            .facts
            .iter()
            .any(|fact| matches!(fact.body, FactBody::MessageFinished { .. }))
    );
    let changes = search_changes(&state, &finished.facts);
    let answer = changes
        .upserts
        .iter()
        .find(|row| row.text == "the searchable answer")
        .expect("the completed answer is indexed");
    assert_eq!(answer.role, "assistant");
}
