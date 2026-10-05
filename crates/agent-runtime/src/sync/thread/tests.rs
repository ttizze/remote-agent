//! Ported from T3 `ThreadStream.test.ts` (every `decideThreadResume` case), the
//! bounded-snapshot case of ws.test.ts and the handoff case of `WireProjection.test.ts`.
use super::*;
use crate::store::tests::{selection, temp_store};
use crate::sync::client_facts;
use crate::sync::history::tests::{at, command_rows, fold_into, thread_id};
use crate::{ActorContext, ActorHandle, CommandOrigin, ThreadHead};
use agent_domain::{
    Command, CommandId, ContextTransferId, Fact, FactBody, HistoricalContext, InteractionMode,
    RuntimeMode, ThreadId, TransferKind,
};

const LARGE_TRANSCRIPT_BYTES: usize = 10 * 1_048_576;
// The projected replay measured well under this; leave room for small envelope additions.
const MAX_PROJECTED_REPLAY_BYTES: u64 = 2_048;

fn head(global_seq: u64) -> ThreadHead {
    ThreadHead {
        thread_seq: global_seq,
        input_seq: global_seq,
        global_seq,
    }
}
fn transfer_opened(transcript: String) -> FactBody {
    FactBody::TransferOpened {
        native_fork: None,
        id: ContextTransferId::new("handoff:wire").unwrap(),
        kind: TransferKind::ProviderHandoff,
        source: thread_id(),
        target: thread_id(),
        instance: "codex".into(),
        boundary: 1,
        history: HistoricalContext {
            messages: vec![],
            context: transcript,
            omitted_items: 0,
            omitted_item_ids: vec![],
        },
    }
}
fn stored(sequence: u64, body: FactBody) -> Arc<[StoredFact]> {
    Arc::from(vec![StoredFact {
        global_seq: sequence,
        thread_seq: sequence,
        fact: Fact { at: at(), body },
    }])
}

#[test]
fn replays_when_the_gap_is_zero() {
    assert_eq!(
        decide_resume(ResumeInput {
            after: 10,
            high_water: 10,
            replay_facts: 0,
            replay_encoded_bytes: 0,
        }),
        ResumePlan::Replay {
            after: 10,
            through: 10
        }
    );
}

#[test]
fn replays_when_the_fact_count_is_within_the_bound() {
    assert_eq!(
        decide_resume(ResumeInput {
            after: 10,
            high_water: 20_000,
            replay_facts: RESUME_MAX_REPLAY_FACTS,
            replay_encoded_bytes: RESUME_MAX_REPLAY_ENCODED_BYTES,
        }),
        ResumePlan::Replay {
            after: 10,
            through: 20_000
        }
    );
}

#[test]
fn falls_back_to_a_snapshot_when_the_fact_count_exceeds_the_bound() {
    assert_eq!(
        decide_resume(ResumeInput {
            after: 10,
            high_water: 20_000,
            replay_facts: RESUME_MAX_REPLAY_FACTS + 1,
            replay_encoded_bytes: 1,
        }),
        ResumePlan::Snapshot
    );
}

#[test]
fn falls_back_to_a_snapshot_when_encoded_replay_bytes_exceed_the_bound() {
    assert_eq!(
        decide_resume(ResumeInput {
            after: 10,
            high_water: 11,
            replay_facts: 1,
            replay_encoded_bytes: RESUME_MAX_REPLAY_ENCODED_BYTES + 1,
        }),
        ResumePlan::Snapshot
    );
}

#[test]
fn rejects_pathological_raw_payloads_before_decoding_them() {
    assert!(replay_raw_payload_safe(RESUME_MAX_RAW_PAYLOAD_BYTES));
    assert!(!replay_raw_payload_safe(RESUME_MAX_RAW_PAYLOAD_BYTES + 1));
    // Raw safety and the encoded size are separate: a tiny replay is
    // transport-safe even when its raw source is not.
    assert_eq!(
        decide_resume(ResumeInput {
            after: 9,
            high_water: 10,
            replay_facts: 1,
            replay_encoded_bytes: 1,
        }),
        ResumePlan::Replay {
            after: 9,
            through: 10
        }
    );
}

#[test]
fn falls_back_to_a_snapshot_when_the_client_cursor_is_ahead_of_the_store() {
    assert_eq!(
        decide_resume(ResumeInput {
            after: 50,
            high_water: 40,
            replay_facts: 0,
            replay_encoded_bytes: 0,
        }),
        ResumePlan::Snapshot
    );
}

#[test]
fn counts_utf8_bytes_across_projected_stream_items() {
    assert_eq!(
        replay_encoded_bytes(&[
            serde_json::json!({ "value": "a" }),
            serde_json::json!({ "value": "🦊" })
        ]),
        (r#"{"value":"a"}"#.len() + r#"{"value":"🦊"}"#.len()) as u64
    );
}

#[test]
fn builds_socket_fallback_snapshots_with_a_bounded_timeline_and_history_cursor() {
    let snapshot = ThreadSnapshot::build(&Arc::new(command_rows(80)), head(44), true);
    let window = snapshot.window.unwrap();
    assert_eq!(snapshot.snapshot_seq, 44);
    assert_eq!(snapshot.state.visible_items().len(), 75);
    assert_eq!(snapshot.state.items.len(), 75);
    assert!(window.history_cursor.is_some());
    assert!(window.has_more_history);
    assert_eq!(window.latest_local_ordinal, Some(80));
    assert!(!window.payload_budget_exceeded);
}

/// T3 shrinks a large tool output on the wire. Here the client form drops a
/// transfer transcript; the raw and encoded checks stay separate the same way.
#[test]
fn rejects_a_10_mib_raw_replay_even_when_its_client_form_fits_the_wire_budget() {
    let raw = stored(10, transfer_opened("x".repeat(LARGE_TRANSCRIPT_BYTES)));
    let raw_payload_bytes = serde_json::to_vec(&raw[0].fact.body).unwrap().len() as u64;
    assert!(raw_payload_bytes > RESUME_MAX_RAW_PAYLOAD_BYTES);
    assert!(!replay_raw_payload_safe(raw_payload_bytes));
    assert!(replay_encoded_bytes(&raw) > RESUME_MAX_REPLAY_ENCODED_BYTES);
    let projected = client_facts(&raw);
    let projected_bytes = replay_encoded_bytes(&projected);
    assert!(projected_bytes <= MAX_PROJECTED_REPLAY_BYTES);
    assert_eq!(
        decide_resume(ResumeInput {
            after: 9,
            high_water: 10,
            replay_facts: projected.len() as u64,
            replay_encoded_bytes: projected_bytes,
        }),
        ResumePlan::Replay {
            after: 9,
            through: 10
        }
    );
}

#[test]
fn keeps_full_thread_snapshot_fallback_unless_the_client_opts_into_bounded_history() {
    assert!(!ThreadSubscribe::default().accept_bounded_snapshot);
    let state = Arc::new(command_rows(3));
    assert!(
        ThreadSnapshot::build(&state, head(3), false)
            .window
            .is_none()
    );
    assert!(
        ThreadSnapshot::build(&state, head(3), true)
            .window
            .is_some()
    );
}

#[test]
fn keeps_copied_handoff_transcripts_out_of_activity_items_and_live_events() {
    let transcript = "PRIVATE_HANDOFF_TRANSCRIPT";
    let mut state = command_rows(3);
    fold_into(&mut state, transfer_opened(transcript.into()));
    let state = Arc::new(state);
    let contains = |value: &dyn erased::Json| value.json().contains(transcript);

    assert!(!contains(&state.activity_items()));
    for bounded in [false, true] {
        assert!(!contains(
            &ThreadSnapshot::build(&state, head(4), bounded).state
        ));
    }
    let live = stored(4, transfer_opened(transcript.into()));
    assert!(!contains(&client_facts(&live)));
    assert!(contains(&live));
    assert!(contains(&state));
}

mod erased {
    pub trait Json {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize + ?Sized> Json for T {
        fn json(&self) -> String {
            serde_json::to_string(self).unwrap()
        }
    }
}

async fn created_thread(id: &str) -> (tempfile::TempDir, ActorHandle, ThreadId) {
    let (dir, store) = temp_store();
    let thread = ThreadId::new(id).unwrap();
    let handle = ActorHandle::spawn(ActorContext::new(store), thread.clone())
        .await
        .unwrap();
    handle
        .dispatch(
            CommandId::new("create").unwrap(),
            Command::Create {
                workspace: None,
                thread: thread.clone(),
                project: "project".into(),
                title: "Thread".into(),
                selection: selection(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    (dir, handle, thread)
}

async fn renames(handle: &ActorHandle, from: usize, count: usize) {
    for index in from..from + count {
        handle
            .dispatch(
                CommandId::new(format!("rename-{index}")).unwrap(),
                Command::Rename {
                    title: format!("Title {index}"),
                },
                CommandOrigin::Client,
            )
            .await
            .unwrap();
    }
}

async fn first_updates(handle: &ActorHandle, options: ThreadSubscribe) -> Vec<ThreadUpdate> {
    let mut subscription = handle.subscribe(options).await.unwrap();
    let mut updates = vec![];
    while let Ok(update) = subscription.updates.try_recv() {
        updates.push(update);
    }
    updates
}

#[tokio::test]
async fn resumes_with_a_replay_until_the_gap_outgrows_the_bound() {
    let (_dir, handle, _) = created_thread("thread:resume").await;
    let after = handle.view().await.unwrap().head.global_seq;
    renames(&handle, 0, 3).await;
    let resume = ThreadSubscribe {
        after_global_seq: Some(after),
        request_completion_marker: true,
        ..ThreadSubscribe::default()
    };
    let updates = first_updates(&handle, resume).await;
    assert!(
        matches!(&updates[..], [ThreadUpdate::Facts(facts), ThreadUpdate::Synchronized] if facts.len() == 3)
    );

    renames(&handle, 3, RESUME_MAX_REPLAY_FACTS as usize - 3).await;
    let updates = first_updates(&handle, resume).await;
    assert!(
        matches!(&updates[..], [ThreadUpdate::Facts(facts), ThreadUpdate::Synchronized] if facts.len() == 128)
    );

    renames(&handle, 128, 1).await;
    let updates = first_updates(&handle, resume).await;
    let [ThreadUpdate::Snapshot(snapshot), ThreadUpdate::Synchronized] = &updates[..] else {
        panic!("{updates:?}")
    };
    assert_eq!(
        snapshot.snapshot_seq,
        handle.view().await.unwrap().head.global_seq
    );
    assert_eq!(snapshot.state.thread.as_ref().unwrap().title, "Title 128");
    assert!(snapshot.window.is_none());

    let caught_up = ThreadSubscribe {
        after_global_seq: Some(snapshot.snapshot_seq),
        ..ThreadSubscribe::default()
    };
    assert!(first_updates(&handle, caught_up).await.is_empty());
}

#[tokio::test]
async fn a_gap_with_the_thread_creation_resumes_from_a_snapshot() {
    let (_dir, handle, _) = created_thread("thread:recreated").await;
    let options = ThreadSubscribe {
        after_global_seq: Some(0),
        accept_bounded_snapshot: true,
        ..ThreadSubscribe::default()
    };
    let updates = first_updates(&handle, options).await;
    let [ThreadUpdate::Snapshot(snapshot)] = &updates[..] else {
        panic!("{updates:?}")
    };
    assert!(snapshot.window.is_some());
}
