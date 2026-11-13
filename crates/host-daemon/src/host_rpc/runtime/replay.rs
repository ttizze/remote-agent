//! Unmodified public recordings from T3's testkit (plus the two PR #55 forks).
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
    assert_eq!(
        recordings
            .iter()
            .filter(|r| r.provider == "deepseek")
            .count(),
        1
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
    for scenario in ["simple", "multi_turn"] {
        let recording = recordings()
            .into_iter()
            .find(|r| r.provider == "claude" && r.scenario == scenario)
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        let (events, mut receiver) = mpsc::channel::<crate::providers::AgentEvent>(256);
        let consumer = service.clone();
        let observing = tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if let AgentChange::Session { session, .. } = &event.change {
                    ensure_session(&consumer, session)
                }
                let result = Runtime::observe(&consumer, event.change, event.execution).await;
                if let Some(applied) = event.applied {
                    let _ = applied.send(result);
                }
            }
        });
        // The literals and completed turn counts are from simple/claude_output.ts
        // and multi_turn/claude_output.ts, independent of the adapter's output.
        let expected: &[&str] = if scenario == "simple" {
            &["fixture simple ok"]
        } else {
            &[
                "first fixture turn complete",
                "second fixture turn complete",
            ]
        };
        let mut changes = service.inner.conversation.storage.changes();
        let (session, running, stop) =
            crate::providers::claude::execution_tests::replay_sdk(&frames(&recording), events)
                .await
                .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if service
                    .inner
                    .conversation
                    .storage
                    .history(&session, 0)
                    .ok()
                    .and_then(|response| response.thread.turns)
                    .is_some_and(|turns| {
                        turns
                            .iter()
                            .filter(|turn| {
                                turn.status == agent_protocol::execution::TurnStatus::Completed
                            })
                            .count()
                            >= expected.len()
                    })
                {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        // Completion is committed. Release the retained idle query, as unloading
        // a finished native session does, without interrupting an active turn.
        stop.cancel();
        running.await.unwrap();
        observing.await.unwrap();
        let response = service
            .inner
            .conversation
            .storage
            .history(&session, 0)
            .unwrap();
        let turns = response.thread.turns.unwrap();
        assert_eq!(turns.len(), expected.len(), "{scenario}");
        for (turn, text) in turns.iter().zip(expected) {
            assert_eq!(
                turn.status,
                agent_protocol::execution::TurnStatus::Completed,
                "{scenario}"
            );
            assert!(
                turn.items.iter().flatten().any(|item| matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                )),
                "{scenario}"
            );
            assert!(turn.items.iter().flatten().any(|item|matches!(item.body(),agent_protocol::items::ItemBody::AssistantText {text:actual,..} if actual.contains(text))),"{scenario}: {text}");
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
}

#[tokio::test]
async fn recorded_codex_frames_cross_the_native_adapter_and_conversation_writer() {
    for recording in recordings().into_iter().filter(|r| r.provider == "codex") {
        let directory = tempfile::tempdir().unwrap();
        let service = HostRpcService::new([], directory.path().join("projects.json"));
        let mut executions = HashMap::<SessionRef, Uuid>::new();
        let mut observed = 0;
        for (index, row) in frames(&recording).into_iter().enumerate() {
            if row["type"] != "emit_inbound" {
                continue;
            }
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
            let line = frame.to_string();
            let message = RpcMessage::parse(&line).unwrap();
            let changes = match message.kind() {
                RpcMessageKind::Notification => codex::event_changes(Uuid::nil(), &message)
                    .unwrap_or_else(|error| panic!("{} frame {index}: {error}", recording.file)),
                RpcMessageKind::Request => {
                    let adapted = crate::providers::requests::codex(
                        format!("recorded-{index}").into(),
                        frame["method"].as_str().unwrap(),
                        &frame["params"],
                    )
                    .unwrap_or_else(|error| panic!("{} frame {index}: {error}", recording.file));
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
            };
            for change in changes {
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
        }
        assert!(observed > 0, "{}", recording.file);
        if recording.scenario == "simple" {
            let session = executions.keys().next().unwrap();
            let response = service
                .inner
                .conversation
                .storage
                .history(session, 0)
                .unwrap();
            let turns = response.thread.turns.unwrap();
            assert_eq!(turns.len(), 1);
            assert_eq!(
                turns[0].status,
                agent_protocol::execution::TurnStatus::Completed
            );
            let items = turns[0].items.as_ref().unwrap();
            // simple/codex_output.ts: one completed turn, user then assistant,
            // and the literal reference reply (independent of this adapter).
            assert!(items.iter().any(|item| matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
            )));
            assert!(items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText {text,..} if text.contains("fixture simple ok"))));
        }
    }
}
