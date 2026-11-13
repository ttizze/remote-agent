//! Public T3 recordings (including PR #55 forks) and a real DSH ACP recording.
//! Dynamic Host IDs are deliberately not part of the reference expectations.
use super::*;
use crate::providers::{RequestOrigin, codex};
use agent_protocol::models::{Thread, ThreadResponse};
use agent_transport::peer::{RpcMessage, RpcMessageKind};
use ring::digest;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Recording {
    scenario: String,
    provider: String,
    file: String,
    sha256: String,
}

fn recordings() -> Vec<Recording> {
    serde_json::from_str(include_str!("../../providers/fixtures/manifest.json")).unwrap()
}
fn frames(recording: &Recording) -> Vec<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/providers/fixtures")
        .join(&recording.file);
    let bytes = std::fs::read(path).unwrap();
    let hash: String = digest::digest(&digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(hash, recording.sha256, "{}", recording.file);
    std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn ensure_session(service: &HostRpcService, session: &SessionRef) {
    if service
        .inner
        .conversation
        .storage
        .lookup(session)
        .unwrap()
        .is_none()
    {
        native_fixture::own(
            service,
            session,
            &ThreadResponse {
                thread: Thread {
                    id: Some(session.clone()),
                    active_provider: session.provider,
                    cwd: Some("/reference".into()),
                    turns: Some(Vec::new()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    }
}

#[test]
fn reference_recordings_keep_their_original_digests() {
    let recordings = recordings();
    assert_eq!(
        recordings.iter().filter(|r| r.provider == "codex").count(),
        30
    );
    assert_eq!(
        recordings.iter().filter(|r| r.provider == "claude").count(),
        41
    );
    for recording in recordings {
        assert!(!frames(&recording).is_empty());
    }
}

#[tokio::test]
async fn recorded_deepseek_acp_conversation_crosses_the_process_and_conversation_writer() {
    use crate::providers::{Agent, SubmissionTarget, deepseek::DeepSeek};
    let recording = recordings()
        .into_iter()
        .find(|r| r.provider == "deepseek")
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/providers/fixtures")
            .join(&recording.file),
        directory.path().join("recording.ndjson"),
    )
    .unwrap();
    let script = directory.path().join("replay.mjs");
    std::fs::write(&script,r#"
import { readFileSync,writeFileSync } from 'node:fs';
import { createInterface } from 'node:readline';
import assert from 'node:assert/strict';
const rows=readFileSync(new URL('recording.ndjson',import.meta.url),'utf8').trim().split('\n').map(JSON.parse);
let cursor=1;
const ids=new Map();
const normalize=request=>({...request,params:{...request.params,
 ...(request.params?.cwd ? {cwd:'<workspace>'}:{}),
 ...(request.method==='initialize' ? {clientInfo:{...request.params.clientInfo,version:'<version>'}}:{})}});
for await(const line of createInterface({input:process.stdin})) {
 const actual=JSON.parse(line), expected=rows[cursor++];
 assert.equal(expected.type,'expect_outbound');
 ids.set(expected.frame.id,actual.id);
 assert.deepEqual(normalize({...actual,id:expected.frame.id}),expected.frame);
 while(rows[cursor]?.type==='emit_inbound') {
   const frame={...rows[cursor++].frame};
   if(frame.id!==undefined) frame.id=ids.get(frame.id);
   process.stdout.write(JSON.stringify(frame)+'\n');
 }
 if(rows[cursor]?.type==='runtime_exit') {writeFileSync(new URL('consumed',import.meta.url),'done');break;}
}
process.stdin.destroy();
"#).unwrap();
    let adapter = DeepSeek::load(
        script,
        directory.path().join("state"),
        Some(directory.path().join("home")),
        // An inert route selector for the local ACP fixture; no API is contacted.
        Some(zeroize::Zeroizing::new("local-acp-fixture".into())),
    )
    .await
    .unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    let mut receiver = adapter.event_stream().unwrap();
    let consumer = service.clone();
    let observing = tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            let result = Runtime::observe(&consumer, event.change, event.execution).await;
            if let Some(applied) = event.applied {
                let _ = applied.send(result);
            }
        }
    });
    let cwd = directory.path().to_str().unwrap();
    let created = adapter.create(cwd, None, None).await.unwrap();
    let native = created.thread.id.as_ref().unwrap().clone();
    native_fixture::own(&service, &native, &created);
    let mut changes = service.inner.conversation.storage.changes();
    adapter
        .submit(
            &op::Submission {
                thread_id: native.clone(),
                client_user_message_id: Uuid::new_v4().to_string().into(),
                input: vec![op::Input::Text {
                    text: "Reply exactly: recorded deepseek ok".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
            SubmissionTarget::Start { cwd },
            false,
            None,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if service
                .inner
                .conversation
                .storage
                .history(&native, 0)
                .unwrap()
                .thread
                .turns
                .is_some_and(|turns| {
                    turns
                        .iter()
                        .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed)
                })
            {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let response = service
        .inner
        .conversation
        .storage
        .history(&native, 0)
        .unwrap();
    let turns = response.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].items.iter().flatten().any(|item|matches!(item.body(),agent_protocol::items::ItemBody::AssistantText {text,..} if text=="recorded deepseek ok")));
    adapter.shutdown().await;
    assert!(
        directory.path().join("consumed").is_file(),
        "every recorded ACP exchange must be consumed"
    );
    drop(adapter);
    observing.await.unwrap();
}

#[tokio::test]
async fn recorded_claude_query_responses_cross_the_sdk_reader_and_conversation_writer() {
    // Expected literals/statuses come from the corresponding T3 output.ts files.
    for (scenario, expected) in [
        ("simple", &["fixture simple ok"][..]),
        ("claude_background_task_after_root", &["L2_STARTED"][..]),
        (
            "claude_background_monitor_wake",
            &["STARTED", "WAKE_DONE"][..],
        ),
    ] {
        let recording = recordings()
            .into_iter()
            .find(|r| r.provider == "claude" && r.scenario == scenario)
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        let (events, mut receiver) = mpsc::channel::<crate::providers::AgentEvent>(256);
        let consumer = service.clone();
        let rosters = Arc::new(Mutex::new(Vec::new()));
        let received = rosters.clone();
        let (settled, completion) = tokio::sync::oneshot::channel();
        let observing = tokio::spawn(async move {
            let mut settled = Some(settled);
            while let Some(event) = receiver.recv().await {
                if let AgentChange::Session { session, change } = &event.change {
                    ensure_session(&consumer, session);
                    if let SessionChange::Background { tasks } = change {
                        received.lock().unwrap().push(tasks.clone());
                    }
                }
                let closed = matches!(event.change, AgentChange::SourceClosed(_));
                let result = Runtime::observe(&consumer, event.change, event.execution).await;
                if let Some(applied) = event.applied {
                    let _ = applied.send(result);
                }
                if closed && let Some(settled) = settled.take() {
                    let _ = settled.send(());
                }
            }
        });
        let (session, running, stop) =
            crate::providers::claude::execution_tests::replay_sdk(&frames(&recording), events)
                .await
                .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), completion)
            .await
            .unwrap_or_else(|error| {
                stop.cancel();
                panic!("{scenario}: {error}")
            })
            .unwrap();
        stop.cancel();
        running.await.unwrap();
        observing.await.unwrap();
        if scenario.starts_with("claude_background") {
            let rosters = rosters.lock().unwrap();
            let first = rosters
                .iter()
                .position(|tasks| !tasks.is_empty())
                .unwrap_or_else(|| panic!("{scenario}: background work must be published"));
            assert!(
                rosters[first + 1..].iter().any(Vec::is_empty),
                "{scenario}: background work must clear"
            );
            assert!(rosters.last().unwrap().is_empty(), "{scenario}");
        }
        let response = service
            .inner
            .conversation
            .storage
            .history(&session, 0)
            .unwrap();
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), expected.len().max(1), "{scenario}");
        for (turn, expected) in turns.iter().zip(expected) {
            assert_eq!(
                turn.status,
                agent_protocol::execution::TurnStatus::Completed,
                "{scenario}"
            );
            let texts: Vec<_> = turn
                .items
                .iter()
                .flatten()
                .filter_map(|item| match item.body() {
                    agent_protocol::items::ItemBody::AssistantText { text, .. } => {
                        Some(text.trim())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(texts, [*expected], "{scenario}: {}", turn.id);
        }
        if scenario == "claude_background_task_after_root" {
            assert!(turns[0].items.iter().flatten().any(|item| matches!(
                item.body(),
                agent_protocol::items::ItemBody::CommandExecution { .. }
            ) && item.status
                == agent_protocol::execution::ItemStatus::Completed));
        }
    }
}

fn assert_recorded_terminal(
    storage: &Storage,
    session: &SessionRef,
    execution: Uuid,
    frame: &Value,
) {
    let info = storage
        .execution(session, execution)
        .unwrap()
        .expect("recorded execution");
    let turn = storage
        .turn(session, &info.attempt.turn)
        .unwrap()
        .expect("canonical turn");
    match frame["params"]["turn"]["status"].as_str().unwrap() {
        "completed" => assert_eq!(turn.state, decision::TurnState::Completed),
        "interrupted" => assert_eq!(turn.state, decision::TurnState::Interrupted),
        status => panic!("unhandled reference terminal status {status}"),
    }
    // turn_interrupt_mid_tool/*_output.ts: finishing a turn settles its tools.
    let history = storage.history(session, 0).unwrap();
    let saved = history
        .thread
        .turns
        .as_ref()
        .unwrap()
        .iter()
        .find(|saved| saved.id == turn.id)
        .unwrap();
    assert!(
        saved
            .items
            .iter()
            .flatten()
            .all(|item| item.status != agent_protocol::execution::ItemStatus::Running)
    );
}

#[tokio::test]
async fn recorded_codex_frames_cross_the_native_adapter_and_conversation_writer() {
    for recording in recordings().into_iter().filter(|r| r.provider == "codex") {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        let mut executions = HashMap::<SessionRef, Uuid>::new();
        let mut requests = HashMap::new();
        let mut requested = Vec::new();
        let mut live_output = None;
        let mut observed = 0;
        for (index, row) in frames(&recording)
            .into_iter()
            .enumerate()
            .filter(|(_, row)| row["type"] == "emit_inbound")
        {
            let frame = &row["frame"];
            if frame["result"]["thread"].is_object() {
                let response =
                    codex::native::parse_thread_response(frame["result"].clone()).unwrap();
                let native = response.thread.id.as_ref().unwrap();
                if service
                    .inner
                    .conversation
                    .storage
                    .lookup(native)
                    .unwrap()
                    .is_none()
                {
                    native_fixture::own(&service, native, &response);
                }
            }
            let changes = decode_codex_frame(index, frame);
            for change in changes {
                let resolved = match &change {
                    AgentChange::Request {
                        session,
                        origin,
                        request,
                    } => {
                        requested.push(request.body.clone());
                        requests.insert(
                            origin.native_id.to_string(),
                            (session.clone(), request.id.clone()),
                        );

                        None
                    }
                    AgentChange::Resolved { native_id, .. } => {
                        requests.remove(&native_id.to_string())
                    }
                    _ => None,
                };
                let execution = match &change {
                    AgentChange::Session { session, change } => {
                        ensure_session(&service, session);
                        if matches!(
                            change,
                            SessionChange::Turn {
                                completed: false,
                                ..
                            }
                        ) {
                            executions.insert(session.clone(), Uuid::new_v4());
                        }
                        executions.get(session).copied()
                    }
                    AgentChange::Request { session, .. } => executions.get(session).copied(),
                    AgentChange::TaskParent { parent, .. } => {
                        ensure_session(&service, parent);
                        None
                    }
                    _ => None,
                };
                Runtime::observe(&service, change, execution)
                    .await
                    .unwrap_or_else(|error| panic!("{} frame {index}: {error}", recording.file));
                if let Some((session, id)) = resolved {
                    assert!(
                        !service
                            .inner
                            .conversation
                            .storage
                            .state(&session)
                            .unwrap()
                            .requests
                            .contains_key(&id),
                        "{}: resolved request",
                        recording.scenario
                    );
                }
                if frame["method"] == "turn/completed" {
                    let session = SessionRef::new(
                        ProviderKind::Codex,
                        frame["params"]["threadId"].as_str().unwrap().into(),
                    )
                    .unwrap();
                    assert_recorded_terminal(
                        &service.inner.conversation.storage,
                        &session,
                        executions[&session],
                        frame,
                    );
                }
                observed += 1;
            }
            if recording.scenario == "command_output_streaming"
                && frame["method"] == "item/commandExecution/outputDelta"
            {
                let session = SessionRef::new(
                    ProviderKind::Codex,
                    frame["params"]["threadId"].as_str().unwrap().into(),
                )
                .unwrap();
                let key = records::OutputKey {
                    item: decision::provider_item_id(
                        executions[&session],
                        frame["params"]["itemId"].as_str().unwrap(),
                    ),
                    session,
                };
                let (tail, _) = service
                    .inner
                    .conversation
                    .outputs
                    .subscribe(&key, None)
                    .unwrap();
                live_output = Some(tail);
                let item = service
                    .inner
                    .conversation
                    .storage
                    .item(&key.session, &key.item)
                    .unwrap()
                    .unwrap();
                assert!(
                    matches!(item.body, agent_protocol::items::ItemBody::CommandExecution {output,..} if output.is_empty())
                );
            }
        }
        assert!(observed > 0, "{}", recording.file);
        let sessions: Vec<_> = executions.into_keys().collect();
        assert_codex_projection(
            &recording.scenario,
            &service.inner.conversation.storage,
            &sessions,
            &requested,
            live_output.as_ref(),
        );
        if recording.scenario == "thread_fork_native_prior_turn" {
            assert_fork_prefix(
                &service.inner.conversation.storage,
                &service.inner.conversation.owners,
                &recording,
            )
            .await;
        }
    }
}

fn assert_codex_projection(
    scenario: &str,
    storage: &Storage,
    sessions: &[SessionRef],
    requested: &[agent_protocol::requests::RequestBody],
    live_output: Option<&agent_protocol::sync::Output>,
) {
    if matches!(
        scenario,
        "plan_questions" | "tool_call_read_only_on_request" | "tool_call_restricted_granular"
    ) {
        assert_eq!(requested.len(), 1, "{scenario}");
    }
    if scenario == "plan_questions" {
        let agent_protocol::requests::RequestBody::Question { questions } = &requested[0] else {
            panic!("expected a question")
        };
        // Bex's existing request contract uses opaque local IDs.
        assert_eq!(
            questions[0].prompt,
            "Should this fixture prefer strict schemas or UI flexibility?"
        );
        assert_eq!(
            questions[0].choices[0].label,
            "Strict schemas (Recommended)"
        );
    }
    // Reference expectations from each scenario's *_output.ts. The replay
    // checks the native-to-Host boundary; command admission has its own tests.
    let expected: &[&str] = match scenario {
        "simple" => &["fixture simple ok"],
        "multi_turn" => &[
            "first fixture turn complete",
            "second fixture turn complete",
        ],
        "message_steering" => &["steering fixture observed"],
        "plan_questions" => &["plan questions fixture complete"],
        "command_output_streaming" => &["command output fixture complete"],
        _ => &[],
    };
    if !expected.is_empty() {
        assert_eq!(sessions.len(), 1, "{}", scenario);
        let session = sessions.first().unwrap();
        let response = storage.history(session, 0).unwrap();
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), expected.len(), "{}", scenario);
        for (turn, reply) in turns.iter().zip(expected) {
            assert_eq!(
                turn.status,
                agent_protocol::execution::TurnStatus::Completed
            );
            let items = turn.items.as_ref().unwrap();
            assert!(items.iter().any(|item| matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
            )));
            assert!(
                items.iter().any(|item| matches!(item.body(),
                agent_protocol::items::ItemBody::AssistantText {text,..} if text.contains(reply))),
                "{}: {reply}",
                scenario
            );
        }
        if scenario == "message_steering" {
            assert_eq!(
                turns[0]
                    .items
                    .iter()
                    .flatten()
                    .filter(|item| matches!(
                        item.body(),
                        agent_protocol::items::ItemBody::UserMessage { .. }
                    ))
                    .count(),
                2
            );
        }
        if scenario == "command_output_streaming" {
            let command = turns[0]
                .items
                .iter()
                .flatten()
                .find(|item| {
                    matches!(
                        item.body(),
                        agent_protocol::items::ItemBody::CommandExecution { .. }
                    )
                })
                .unwrap();
            assert_eq!(
                command.status,
                agent_protocol::execution::ItemStatus::Completed
            );
            let agent_protocol::items::ItemBody::CommandExecution { output, .. } = command.body()
            else {
                unreachable!()
            };
            let live = live_output.expect("the native recording must stream live command output");
            assert!(live.sequence > 0);
            for text in [output.as_str(), live.text.as_str()] {
                assert!(text.contains("progress 100%"));
                assert!(!text.contains("progress 50%"));
                for line in ["stream line 2", "stream line 3", "stream line 4"] {
                    assert!(text.contains(line));
                }
            }
            assert!(output.trim_end().ends_with(live.text.trim_end()));
        }
    }
    if scenario == "proposed_plan" {
        let session = sessions.first().unwrap();
        let response = storage.history(session, 0).unwrap();
        let turns = response.thread.turns.unwrap();
        assert!(turns.iter().flat_map(|turn| turn.items.iter().flatten()).any(|item|
            matches!(item.body(), agent_protocol::items::ItemBody::Plan {text}
                if text.to_lowercase().contains("replay") && text.to_lowercase().contains("fixture"))
            && item.status == agent_protocol::execution::ItemStatus::Completed));
    }
}

async fn assert_fork_prefix(storage: &Storage, owners: &Arc<Owners>, recording: &Recording) {
    let rows = frames(recording);
    let fork = rows
        .iter()
        .find(|row| row["type"] == "expect_outbound" && row["frame"]["method"] == "thread/fork")
        .unwrap();
    let source = SessionRef::new(
        ProviderKind::Codex,
        fork["frame"]["params"]["threadId"].as_str().unwrap().into(),
    )
    .unwrap();
    let history = storage.history(&source, 0).unwrap();
    let through = history.thread.turns.as_ref().unwrap()[0].id.clone();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    owners
        .fork(&source, target.clone(), through, 100)
        .await
        .unwrap();
    let turns = storage.history(&target, 0).unwrap().thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    let texts = turns[0]
        .items
        .iter()
        .flatten()
        .map(|item| serde_json::to_string(item.body()).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(turns[0].items.iter().flatten().any(|item| matches!(
        item.body(), agent_protocol::items::ItemBody::AssistantText { text, .. }
            if text.contains("fork boundary alpha")
    )));
    assert!(!texts.contains("fork boundary beta"));
}

fn decode_codex_frame(index: usize, frame: &Value) -> Vec<AgentChange> {
    let line = frame.to_string();
    let message = RpcMessage::parse(&line).unwrap();
    match message.kind() {
        RpcMessageKind::Notification => codex::event_changes(Uuid::nil(), &message)
            .unwrap_or_else(|error| panic!("Codex recording frame {index}: {error}")),
        RpcMessageKind::Request => {
            let adapted = crate::providers::requests::codex(
                format!("recorded-{index}").into(),
                frame["method"].as_str().unwrap(),
                &frame["params"],
            )
            .unwrap_or_else(|error| panic!("Codex recording frame {index}: {error}"));
            vec![AgentChange::Request {
                session: SessionRef::new(
                    ProviderKind::Codex,
                    frame["params"]["threadId"].as_str().unwrap().into(),
                )
                .unwrap(),
                origin: RequestOrigin {
                    instance: Uuid::nil(),
                    native_id: frame["id"].clone(),
                    provider: ProviderKind::Codex,
                    source: Arc::new(native_fixture::AnswerResource),
                },
                request: adapted.request,
            }]
        }
        _ => Vec::new(),
    }
}
