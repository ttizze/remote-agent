use super::*;
use crate::sync::ThreadSync;
use crate::sync::fixtures::{at, run, thread_id, thread_state};
use agent_domain::{
    InputIntent, Message, MessageAuthor, MessageId, RunId, WORKTREE_SETUP_STAGE_ORDER,
    WorktreeSetupScript,
};
use rstest::rstest;
use std::sync::Arc;

const NOW: i64 = 1_781_913_600_000;

fn stage(id: WorktreeSetupStageId, status: WorktreeSetupStageStatus) -> WorktreeSetupStage {
    let open = matches!(
        status,
        WorktreeSetupStageStatus::Running | WorktreeSetupStageStatus::Pending
    );
    WorktreeSetupStage {
        id,
        status,
        started_at: Some(at()),
        ended_at: (!open).then(at),
        percent: None,
        detail: None,
        tail: vec![],
    }
}

fn base() -> WorktreeSetupSnapshot {
    WorktreeSetupSnapshot {
        thread: thread_id(),
        phase: WorktreeSetupPhase::Running,
        started_at: at(),
        ended_at: None,
        branch: Some("feature".into()),
        base_ref: Some("main".into()),
        worktree_path: None,
        setup_script: None,
        stages: vec![
            stage(
                WorktreeSetupStageId::Checkout,
                WorktreeSetupStageStatus::Running,
            ),
            stage(
                WorktreeSetupStageId::Agent,
                WorktreeSetupStageStatus::Pending,
            ),
        ],
        error: None,
        sequence: 1,
    }
}

fn settled_done() -> WorktreeSetupSnapshot {
    WorktreeSetupSnapshot {
        phase: WorktreeSetupPhase::Done,
        ended_at: Some(at()),
        stages: vec![
            stage(
                WorktreeSetupStageId::Checkout,
                WorktreeSetupStageStatus::Done,
            ),
            stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Done,
            ),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..base()
    }
}

fn other_thread() -> ThreadId {
    ThreadId::new("other-thread").unwrap()
}

fn context(layout: SetupCardLayout, turn_started: bool) -> CardContext {
    CardContext {
        layout,
        turn_started,
        working_since_ms: None,
        now_ms: at().millis() + 4_200,
    }
}

fn script(terminal_id: Option<&str>) -> Option<WorktreeSetupScript> {
    Some(WorktreeSetupScript {
        name: "Install".into(),
        command: "bun install".into(),
        terminal_id: terminal_id.map(Into::into),
    })
}

#[test]
fn keeps_the_setup_identity_after_its_card_retires_so_the_working_header_can_take_over() {
    let running = base();
    let done = WorktreeSetupSnapshot {
        phase: WorktreeSetupPhase::Done,
        sequence: 2,
        ..base()
    };
    let held = resolve_setup_snapshot(&thread_id(), Some(&done), Some(&running));
    assert_eq!(held, Some(&done));
    assert_eq!(resolve_visible_setup(held, false, false), Some(&done));
    assert_eq!(resolve_visible_setup(held, true, false), None);
    assert_eq!(
        resolve_setup_snapshot(&thread_id(), None, held),
        Some(&done)
    );
}

#[rstest]
#[case(WorktreeSetupPhase::Failed)]
#[case(WorktreeSetupPhase::Cancelled)]
fn retains_settled_details_after_the_stream_closes(#[case] phase: WorktreeSetupPhase) {
    let running = base();
    let settled = WorktreeSetupSnapshot {
        phase,
        sequence: 3,
        ..base()
    };
    let held = resolve_setup_snapshot(&thread_id(), Some(&settled), Some(&running));
    assert_eq!(
        resolve_setup_snapshot(&thread_id(), None, held),
        Some(&settled)
    );
    assert_eq!(
        resolve_setup_snapshot(&thread_id(), Some(&running), held),
        Some(&settled)
    );
    assert_eq!(resolve_visible_setup(held, false, false), Some(&settled));
    assert_eq!(resolve_visible_setup(held, false, true), None);
}

#[test]
fn does_not_carry_a_previous_threads_progress_across_navigation() {
    let running = base();
    assert_eq!(
        resolve_setup_snapshot(&other_thread(), Some(&running), Some(&running)),
        None
    );
    let other = WorktreeSetupSnapshot {
        thread: other_thread(),
        ..base()
    };
    assert_eq!(
        resolve_setup_snapshot(&other_thread(), Some(&other), Some(&running)),
        Some(&other)
    );
}

#[test]
fn keeps_setup_presentation_continuous_until_the_provider_handoff() {
    let thread = thread_id();
    let base = base();
    let progress = |local_preparing, run_status, latest, held| {
        resolve_setup_progress(&thread, local_preparing, run_status, latest, held)
    };

    assert!(progress(true, None, None, None).1);
    assert!(progress(false, Some(RunStatus::Preparing), None, None).1);
    assert_eq!(
        progress(false, Some(RunStatus::Preparing), Some(&base), None).0,
        Some(&base)
    );
    assert!(progress(false, Some(RunStatus::Starting), Some(&base), None).1);
    let handed_off = WorktreeSetupSnapshot {
        sequence: 2,
        stages: vec![
            stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Running,
            ),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..base.clone()
    };
    assert_eq!(
        progress(
            false,
            Some(RunStatus::Starting),
            Some(&handed_off),
            Some(&base)
        ),
        (Some(&handed_off), false)
    );
    assert_eq!(
        progress(false, Some(RunStatus::Running), None, Some(&handed_off)).0,
        Some(&handed_off)
    );
}

#[test]
fn uses_streamed_setup_progress_immediately_without_reverting_to_an_older_held_snapshot() {
    let thread = thread_id();
    let base = base();
    let newest = WorktreeSetupSnapshot {
        sequence: 9,
        ..settled_done()
    };
    let resolve = |latest, held| {
        resolve_setup_progress(&thread, false, Some(RunStatus::Running), latest, held)
    };
    assert_eq!(resolve(Some(&newest), Some(&base)), (Some(&newest), false));
    assert_eq!(resolve(Some(&base), Some(&newest)), (Some(&newest), false));
    let other = WorktreeSetupSnapshot {
        thread: ThreadId::new("another-thread").unwrap(),
        ..base.clone()
    };
    assert_eq!(resolve(Some(&other), Some(&other)), (None, false));
}

#[rstest]
#[case(WorktreeSetupPhase::Failed)]
#[case(WorktreeSetupPhase::Cancelled)]
fn does_not_keep_settled_setup_in_the_preparing_state(#[case] phase: WorktreeSetupPhase) {
    let snapshot = WorktreeSetupSnapshot { phase, ..base() };
    assert_eq!(
        resolve_setup_progress(
            &thread_id(),
            false,
            Some(RunStatus::Failed),
            Some(&snapshot),
            Some(&base()),
        ),
        (Some(&snapshot), false)
    );
}

#[test]
fn shows_a_running_setup_and_drops_a_clean_one_once_the_turn_started() {
    let base = base();
    let settled = settled_done();
    assert_eq!(
        resolve_visible_setup(Some(&base), false, false),
        Some(&base)
    );
    assert_eq!(
        resolve_visible_setup(Some(&settled), false, false),
        Some(&settled)
    );
    assert_eq!(resolve_visible_setup(Some(&settled), true, false), None);
    assert_eq!(resolve_visible_setup(None, true, false), None);
}

#[test]
fn keeps_a_failed_script_a_failed_setup_and_a_cancelled_setup_visible() {
    let script_failed = WorktreeSetupSnapshot {
        stages: vec![
            stage(
                WorktreeSetupStageId::Checkout,
                WorktreeSetupStageStatus::Done,
            ),
            stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Failed,
            ),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..settled_done()
    };
    let visible =
        |snapshot, follow_up_sent| resolve_visible_setup(Some(snapshot), true, follow_up_sent);
    assert_eq!(visible(&script_failed, false), Some(&script_failed));
    let failed = WorktreeSetupSnapshot {
        phase: WorktreeSetupPhase::Failed,
        error: Some("git exploded".into()),
        ..settled_done()
    };
    assert_eq!(visible(&failed, false), Some(&failed));
    let cancelled = WorktreeSetupSnapshot {
        phase: WorktreeSetupPhase::Cancelled,
        ..settled_done()
    };
    assert_eq!(visible(&cancelled, false), Some(&cancelled));

    assert_eq!(visible(&script_failed, true), None);
    assert_eq!(visible(&failed, true), None);
    assert_eq!(visible(&cancelled, true), None);
    let done = settled_done();
    assert_eq!(visible(&done, true), None);
    let still_running = WorktreeSetupSnapshot {
        stages: vec![
            stage(
                WorktreeSetupStageId::Checkout,
                WorktreeSetupStageStatus::Done,
            ),
            stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Running,
            ),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..base()
    };
    assert_eq!(visible(&still_running, true), Some(&still_running));
}

#[test]
fn prefers_whichever_snapshot_is_newer_by_sequence() {
    let pick = |live: WorktreeSetupSnapshot, held: WorktreeSetupSnapshot| {
        resolve_setup_snapshot(&thread_id(), Some(&live), Some(&held))
            .filter(|snapshot| resolve_visible_setup(Some(snapshot), false, false).is_some())
            .cloned()
    };
    let newer_done = WorktreeSetupSnapshot {
        sequence: 7,
        ..settled_done()
    };
    assert_eq!(
        pick(
            WorktreeSetupSnapshot {
                sequence: 3,
                ..base()
            },
            newer_done.clone()
        ),
        Some(newer_done)
    );
    let newest_done = WorktreeSetupSnapshot {
        sequence: 9,
        ..settled_done()
    };
    assert_eq!(
        pick(
            newest_done.clone(),
            WorktreeSetupSnapshot {
                sequence: 1,
                ..base()
            }
        ),
        Some(newest_done)
    );
}

#[test]
fn labels_stages_in_the_host_order() {
    let labels: Vec<_> = WORKTREE_SETUP_STAGE_ORDER
        .into_iter()
        .map(stage_label)
        .collect();
    assert_eq!(
        labels,
        [
            "Fetch base branch",
            "Check out files",
            "Init submodules",
            "Run setup script",
            "Start agent",
        ]
    );
}

#[test]
fn a_running_desktop_card_shows_progress_output_and_cancel() {
    let mut checkout = stage(
        WorktreeSetupStageId::Checkout,
        WorktreeSetupStageStatus::Running,
    );
    checkout.percent = Some(42);
    checkout.detail = Some("120 files".into());
    let mut setup = stage(
        WorktreeSetupStageId::SetupScript,
        WorktreeSetupStageStatus::Running,
    );
    setup.tail = vec!["one".into(), "two".into()];
    let snapshot = WorktreeSetupSnapshot {
        setup_script: script(None),
        stages: vec![
            stage(
                WorktreeSetupStageId::Fetch,
                WorktreeSetupStageStatus::Skipped,
            ),
            checkout,
            setup,
            stage(
                WorktreeSetupStageId::Agent,
                WorktreeSetupStageStatus::Pending,
            ),
        ],
        ..base()
    };

    let card = setup_card(&snapshot, &context(SetupCardLayout::Desktop, false));

    assert_eq!(card.title, "Setting up worktree…");
    assert!(!card.show_header);
    assert!(card.show_in_timeline && card.owns_working_slot && card.show_stages);
    assert_eq!(card.elapsed.as_deref(), Some("4.2s"));
    let rows: Vec<_> = card
        .stages
        .iter()
        .map(|stage| {
            (
                stage.label.as_str(),
                stage.trailing.as_deref(),
                stage.elapsed.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("Fetch base branch", Some("skipped"), None),
            ("Check out files", Some("42%"), Some("4.2s")),
            ("Install", None, Some("4.2s")),
            ("Start agent", None, None),
        ]
    );
    assert_eq!(
        card.stages[2].output,
        Some(SetupOutputTail {
            lines: vec!["".into(), "".into(), "one".into(), "two".into()],
            failed: false,
        })
    );
    assert!(card.can_cancel && card.can_work_locally);
    assert_eq!(card.open_terminal_id, None);
    assert_eq!(card.background_script, None);
    let details: Vec<_> = card
        .details
        .iter()
        .map(|detail| (detail.label.as_str(), detail.value.as_str()))
        .collect();
    assert_eq!(
        details,
        [
            ("Branch", "feature"),
            ("Base", "main"),
            ("Setup", "bun install")
        ]
    );
}

#[test]
fn open_terminal_needs_a_script_terminal_that_has_started() {
    let with_stage = |status| WorktreeSetupSnapshot {
        setup_script: script(Some("setup-install")),
        stages: vec![stage(WorktreeSetupStageId::SetupScript, status)],
        ..base()
    };
    let desktop = context(SetupCardLayout::Desktop, false);
    assert_eq!(
        setup_card(&with_stage(WorktreeSetupStageStatus::Pending), &desktop).open_terminal_id,
        None
    );
    assert_eq!(
        setup_card(&with_stage(WorktreeSetupStageStatus::Running), &desktop)
            .open_terminal_id
            .as_deref(),
        Some("setup-install")
    );
    let mobile = context(SetupCardLayout::Mobile, false);
    assert_eq!(
        setup_card(&with_stage(WorktreeSetupStageStatus::Running), &mobile).open_terminal_id,
        None
    );
}

#[test]
fn a_failed_setup_brings_its_header_error_and_failed_output() {
    let mut setup = stage(
        WorktreeSetupStageId::SetupScript,
        WorktreeSetupStageStatus::Failed,
    );
    setup.tail = vec!["1".into(), "2".into(), "3".into(), "4".into(), "5".into()];
    setup.detail = Some("exit 1".into());
    let snapshot = WorktreeSetupSnapshot {
        phase: WorktreeSetupPhase::Failed,
        ended_at: Some(at()),
        error: Some("Setup script exited with 1".into()),
        stages: vec![setup],
        ..base()
    };

    let card = setup_card(&snapshot, &context(SetupCardLayout::Desktop, false));

    assert_eq!(card.title, "Worktree setup failed");
    assert_eq!(card.tone, SetupTone::Destructive);
    assert!(card.show_header);
    assert_eq!(card.elapsed.as_deref(), Some("1ms"));
    assert_eq!(card.error.as_deref(), Some("Setup script exited with 1"));
    assert_eq!(card.stages[0].trailing.as_deref(), Some("exit 1"));
    assert_eq!(
        card.stages[0].output,
        Some(SetupOutputTail {
            lines: vec!["2".into(), "3".into(), "4".into(), "5".into()],
            failed: true,
        })
    );
    assert!(!card.can_cancel);
}

#[test]
fn a_settled_desktop_card_collapses_after_the_handoff() {
    let snapshot = WorktreeSetupSnapshot {
        stages: vec![
            stage(
                WorktreeSetupStageId::Checkout,
                WorktreeSetupStageStatus::Done,
            ),
            stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Failed,
            ),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..settled_done()
    };

    let card = setup_card(&snapshot, &context(SetupCardLayout::Desktop, true));

    assert_eq!(card.title, "Worktree ready, setup script failed");
    assert_eq!(card.tone, SetupTone::Warning);
    assert!(card.handed_off && !card.show_stages && !card.show_header);
    assert!(!card.owns_working_slot);
    assert_eq!(
        card.summary,
        Some(SetupSummaryRow {
            status: SetupStageStatus::Failed,
            label: "Worktree ready, setup script failed".into(),
            elapsed: Some("1ms".into()),
        })
    );
    assert!(!card.can_cancel);
}

#[test]
fn a_script_running_after_the_handoff_moves_to_the_working_header() {
    let snapshot = WorktreeSetupSnapshot {
        setup_script: script(None),
        stages: vec![
            stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Running,
            ),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..base()
    };

    let desktop = setup_card(&snapshot, &context(SetupCardLayout::Desktop, true));
    assert!(!desktop.show_in_timeline);
    assert_eq!(desktop.background_script.as_deref(), Some("Install"));
    assert!(!desktop.can_cancel);

    let mobile = setup_card(&snapshot, &context(SetupCardLayout::Mobile, true));
    assert_eq!(mobile.title, "Setup continues…");
    assert_eq!(mobile.elapsed, None);
    assert!(!mobile.show_stages);
    let working = setup_card(
        &snapshot,
        &CardContext {
            working_since_ms: Some(at().millis()),
            now_ms: at().millis() + 64_000,
            ..context(SetupCardLayout::Mobile, true)
        },
    );
    assert_eq!(working.title, "Working for 1m 4s");
}

#[rstest]
#[case(WorktreeSetupPhase::Done, false, "Worktree ready")]
#[case(WorktreeSetupPhase::Done, true, "Setup script failed")]
#[case(WorktreeSetupPhase::Failed, false, "Worktree setup failed")]
#[case(WorktreeSetupPhase::Cancelled, false, "Worktree setup cancelled")]
fn mobile_titles_name_the_outcome(
    #[case] phase: WorktreeSetupPhase,
    #[case] script_failed: bool,
    #[case] title: &str,
) {
    let script_status = if script_failed {
        WorktreeSetupStageStatus::Failed
    } else {
        WorktreeSetupStageStatus::Done
    };
    let snapshot = WorktreeSetupSnapshot {
        phase,
        stages: vec![
            stage(WorktreeSetupStageId::SetupScript, script_status),
            stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..settled_done()
    };
    let card = setup_card(&snapshot, &context(SetupCardLayout::Mobile, false));
    assert_eq!(card.title, title);
    assert_eq!(card.stages.len(), 1);
}

#[test]
fn mobile_cancel_waits_only_until_the_turn_starts() {
    let card = |turn_started| setup_card(&base(), &context(SetupCardLayout::Mobile, turn_started));
    assert!(card(false).can_cancel);
    assert!(!card(true).can_cancel);
}

#[rstest]
#[case(-5, "0ms")]
#[case(0, "1ms")]
#[case(850, "850ms")]
#[case(1_050, "1.1s")]
#[case(9_960, "10s")]
#[case(12_400, "12s")]
#[case(60_000, "1m")]
#[case(3_787_000, "1h 3m 7s")]
fn formats_durations(#[case] duration_ms: i64, #[case] text: &str) {
    assert_eq!(format_duration(duration_ms), text);
}

fn user_message(id: &str, run: &str) -> Message {
    Message {
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: Some(RunId::new(run).unwrap()),
        role: Role::User,
        text: id.into(),
        attachments: vec![],
        intent: InputIntent::TurnStart,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        created_at: at(),
        updated_at: at(),
        context: None,
    }
}

fn snapshot_with(state: State, setup: WorktreeSetupSnapshot) -> Snapshot {
    let mut sync = ThreadSync::default();
    sync.state = Some(Arc::new(state));
    let mut snapshot = Snapshot::default();
    snapshot.threads.insert(thread_id(), Arc::new(sync));
    snapshot.setups.insert(thread_id(), setup);
    snapshot
}

#[test]
fn the_thread_view_retires_a_clean_setup_once_the_first_turn_runs() {
    let mut state = thread_state("Thread");
    state.runs.push(run("run-1", 1, RunStatus::Running));
    state.messages.push(user_message("first", "run-1"));

    let view = setup_view(
        &snapshot_with(state.clone(), settled_done()),
        &thread_id(),
        SetupCardLayout::Desktop,
        NOW,
    );
    assert_eq!(view.card, None);
    assert!(!view.preparing_worktree && !view.blocks_send);

    let mut preparing = state.clone();
    preparing.runs[0].status = RunStatus::Preparing;
    preparing.runs[0].started_at = None;
    let view = setup_view(
        &snapshot_with(preparing, base()),
        &thread_id(),
        SetupCardLayout::Desktop,
        NOW,
    );
    assert_eq!(view.card.map(|card| card.sequence), Some(1));
    assert!(view.preparing_worktree && view.blocks_send);
}

#[test]
fn the_thread_view_retires_a_failed_setup_after_a_follow_up() {
    let mut state = thread_state("Thread");
    state.runs.push(run("run-1", 1, RunStatus::Completed));
    state.runs.push(run("run-2", 2, RunStatus::Queued));
    state.messages.push(user_message("first", "run-1"));
    let failed = WorktreeSetupSnapshot {
        phase: WorktreeSetupPhase::Failed,
        ..settled_done()
    };
    let view = |state: &State| {
        setup_view(
            &snapshot_with(state.clone(), failed.clone()),
            &thread_id(),
            SetupCardLayout::Mobile,
            NOW,
        )
    };
    assert!(view(&state).card.is_some());

    state.messages.push(user_message("second", "run-2"));
    assert_eq!(view(&state).card, None);
}
