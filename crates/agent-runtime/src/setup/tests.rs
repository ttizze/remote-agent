use super::*;
use crate::SystemClock;
use WorktreeSetupStageId::{Agent, Checkout, Fetch, SetupScript};
use WorktreeSetupStageStatus::{Done, Running};

fn thread() -> ThreadId {
    ThreadId::new("thread-1").unwrap()
}

fn tracker() -> Arc<SetupTracker> {
    SetupTracker::new(Arc::new(SystemClock))
}

#[tokio::test]
async fn records_stage_transitions_checkout_progress_and_the_final_phase() {
    let tracker = tracker();
    tracker.begin(
        &thread(),
        Some("feature".into()),
        Some("main".into()),
        &[Checkout, Fetch, Agent],
        None,
    );
    let initial = tracker.get(&thread()).unwrap();
    // Stages are reordered into the canonical setup order.
    assert_eq!(
        initial
            .stages
            .iter()
            .map(|stage| stage.id)
            .collect::<Vec<_>>(),
        [Fetch, Checkout, Agent]
    );
    assert_eq!(initial.phase, WorktreeSetupPhase::Running);

    tracker.stage_status(&thread(), Fetch, Running, None);
    tracker.stage_status(
        &thread(),
        Fetch,
        Done,
        Some(Some("origin/main at abc1234".into())),
    );
    tracker.stage_status(&thread(), Checkout, Running, None);
    tracker.stage(&thread(), Checkout, |stage| {
        stage.percent = Some(42);
        stage.detail = Some("42 / 100 files".into());
    });
    tracker.finish(&thread(), WorktreeSetupPhase::Failed, Some("boom"));

    let last = tracker.get(&thread()).unwrap();
    assert_eq!(last.phase, WorktreeSetupPhase::Failed);
    assert_eq!(last.error.as_deref(), Some("boom"));
    let [fetch, checkout, agent] = &last.stages[..] else {
        panic!()
    };
    assert_eq!(fetch.status, Done);
    assert_eq!(fetch.detail.as_deref(), Some("origin/main at abc1234"));
    assert!(fetch.started_at.is_some() && fetch.ended_at.is_some());
    // A stage still running when the setup fails is marked failed.
    assert_eq!(checkout.status, WorktreeSetupStageStatus::Failed);
    assert_eq!(checkout.percent, Some(42));
    assert_eq!(agent.status, WorktreeSetupStageStatus::Pending);
    assert!(last.sequence > initial.sequence);
}

#[tokio::test]
async fn stream_emits_the_current_snapshot_first_and_then_only_newer_ones() {
    let tracker = tracker();
    tracker.begin(&thread(), None, None, &[Agent], None);
    let mut stream = tracker.subscribe(&thread());
    let mut sequences = vec![stream.borrow_and_update().as_ref().unwrap().sequence];
    tracker.stage_status(&thread(), Agent, Running, None);
    tracker.append_tail(&thread(), Agent, "line 1");
    while sequences.last() != Some(&2) {
        stream.changed().await.unwrap();
        sequences.push(stream.borrow_and_update().as_ref().unwrap().sequence);
    }
    // Delivery is latest-value: intermediates may be skipped, never reordered.
    assert!(sequences.is_sorted());
    assert_eq!(stream.borrow().as_ref().unwrap().stages[0].tail, ["line 1"]);
}

#[tokio::test]
async fn stream_never_steps_back_behind_the_snapshot_it_started_from() {
    let tracker = tracker();
    tracker.begin(&thread(), None, None, &[Agent], None);
    tracker.stage_status(&thread(), Agent, Running, None);
    tracker.stage_status(&thread(), Agent, Done, None);
    let mut stream = tracker.subscribe(&thread());
    assert_eq!(stream.borrow_and_update().as_ref().unwrap().sequence, 2);
    tracker.finish(&thread(), WorktreeSetupPhase::Done, None);
    stream.changed().await.unwrap();
    let last = stream.borrow().clone().unwrap();
    assert!(last.sequence >= 2);
    assert_eq!(last.phase, WorktreeSetupPhase::Done);
}

#[tokio::test]
async fn a_new_setup_on_the_same_thread_keeps_sequences_increasing() {
    let tracker = tracker();
    tracker.begin(&thread(), Some("first".into()), None, &[Agent], None);
    tracker.finish(&thread(), WorktreeSetupPhase::Failed, Some("boom"));
    let failed = tracker.get(&thread()).unwrap().sequence;
    // A stream opened on the failed setup still receives the next one.
    let mut stream = tracker.subscribe(&thread());
    stream.borrow_and_update();
    tracker.begin(&thread(), Some("second".into()), None, &[Agent], None);
    stream.changed().await.unwrap();
    let last = stream.borrow().clone().unwrap();
    assert_eq!(last.phase, WorktreeSetupPhase::Running);
    assert!(last.sequence > failed);
}

#[tokio::test(start_paused = true)]
async fn finished_setups_are_dropped_after_the_retention_window() {
    let tracker = tracker();
    tracker.begin(&thread(), None, None, &[Agent], None);
    tracker.finish(&thread(), WorktreeSetupPhase::Done, None);
    assert_eq!(
        tracker.get(&thread()).unwrap().phase,
        WorktreeSetupPhase::Done
    );
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert!(tracker.get(&thread()).is_none());
}

#[tokio::test]
async fn cancel_stops_the_setup_and_reports_whether_one_was_running() {
    let tracker = tracker();
    let token = CancellationToken::new();
    tracker.begin(&thread(), None, None, &[Agent], Some(token.clone()));
    let unwinding = {
        let tracker = tracker.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            tracker.finish(&thread(), WorktreeSetupPhase::Cancelled, None);
        })
    };
    assert!(tracker.cancel(&thread()).await);
    // cancel returns only after the setup has unwound.
    assert_eq!(
        tracker.get(&thread()).unwrap().phase,
        WorktreeSetupPhase::Cancelled
    );
    unwinding.await.unwrap();
    assert!(!tracker.cancel(&thread()).await);
    assert!(!tracker.cancel(&ThreadId::new("unknown").unwrap()).await);
}

#[tokio::test]
async fn mark_uncancellable_makes_a_later_cancel_a_no_op_while_the_setup_keeps_running() {
    let tracker = tracker();
    let token = CancellationToken::new();
    tracker.begin(&thread(), None, None, &[Agent], Some(token.clone()));
    tracker.mark_uncancellable(&thread());
    assert!(!tracker.cancel(&thread()).await);
    assert!(!token.is_cancelled());
    assert_eq!(
        tracker.get(&thread()).unwrap().phase,
        WorktreeSetupPhase::Running
    );
}

#[tokio::test]
async fn clamps_free_text_to_the_contract_limits_before_publishing() {
    let tracker = tracker();
    tracker.begin(&thread(), None, None, &[Checkout, SetupScript], None);
    let long = "x".repeat(2_000);
    tracker.stage_status(&thread(), Checkout, Done, Some(Some(long.clone())));
    tracker.stage(&thread(), SetupScript, |stage| {
        stage.detail = Some(long.clone())
    });
    tracker.append_tail(&thread(), SetupScript, &long);
    tracker.finish(&thread(), WorktreeSetupPhase::Failed, Some(&long));
    let snapshot = tracker.get(&thread()).unwrap();
    let units = |text: &str| text.chars().map(char::len_utf16).sum::<usize>();
    assert_eq!(units(snapshot.stages[0].detail.as_deref().unwrap()), 200);
    assert_eq!(units(snapshot.stages[1].detail.as_deref().unwrap()), 200);
    assert_eq!(units(&snapshot.stages[1].tail[0]), 400);
    assert_eq!(units(snapshot.error.as_deref().unwrap()), 1000);
}
