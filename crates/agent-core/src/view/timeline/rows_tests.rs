use super::*;
use crate::sync::ThreadStatus;
use crate::view::work_log::fixtures::*;
use agent_domain::RunStatus;
use agent_domain::{
    Checkpoint, CheckpointFile, CheckpointId, CheckpointStatus, CommandId, Role, TurnItemId,
};

fn thread() -> ThreadId {
    thread_id()
}

/// A finished turn: the user's message, a command and the answer.
fn finished_state() -> State {
    let mut state = state(vec![
        user_message("item-user", "message-user")
            .run("run-1")
            .ordinal(1)
            .started("2026-09-06T10:00:00Z"),
        command("item-command", "cargo test")
            .run("run-1")
            .ordinal(2)
            .started("2026-09-06T10:00:01Z")
            .output_omitted(),
        assistant_message("item-answer", "message-answer")
            .run("run-1")
            .ordinal(3)
            .started("2026-09-06T10:00:02Z"),
    ]);
    let mut first = run("run-1", 1, RunStatus::Completed);
    first.started_at = Some(timestamp("2026-09-06T10:00:00Z"));
    first.completed_at = Some(timestamp("2026-09-06T10:00:03Z"));
    state.runs.push(first);
    let mut user = message("message-user", Role::User, "Run the tests");
    user.run = Some(RunId::new("run-1").unwrap());
    let mut answer = message("message-answer", Role::Assistant, "All green.");
    answer.run = Some(RunId::new("run-1").unwrap());
    state.messages.extend([user, answer]);
    state.checkpoints.push(Checkpoint {
        status: CheckpointStatus::Ready,
        scope: None,
        id: CheckpointId::new("checkpoint-1").unwrap(),
        run: Some(RunId::new("run-1").unwrap()),
        run_ordinal: 1,
        native_heads: Default::default(),
        file_ref: "checkpoint-1".into(),
        files: vec![CheckpointFile {
            path: "src/lib.rs".into(),
            kind: "modified".into(),
            additions: 3,
            deletions: 1,
        }],
    });
    state
}

fn synced(state: State, cursor: u64) -> ThreadSync {
    let mut sync = ThreadSync::default();
    sync.state = Some(Arc::new(state));
    sync.cursor = cursor;
    sync.status = ThreadStatus::Live;
    sync
}

fn source<'a>(
    thread: &'a ThreadId,
    sync: &'a ThreadSync,
    shell: Option<&'a ThreadShell>,
    pending: &'a [PendingMessage],
) -> TimelineSource<'a> {
    TimelineSource {
        thread,
        sync,
        shell,
        pending,
        setup: None,
        send_started_at: None,
        now_ms: 0,
    }
}

fn layout(layout: TimelineLayout) -> TimelineOptions {
    TimelineOptions {
        layout,
        ..TimelineOptions::default()
    }
}

fn kinds(rows: &[TimelineRow]) -> Vec<(String, &'static str)> {
    rows.iter()
        .map(|row| {
            let kind = match &row.kind {
                TimelineRowKind::UserMessage(_) => "user",
                TimelineRowKind::AssistantMessage(_) => "assistant",
                TimelineRowKind::AssistantMeta { .. } => "assistant-meta",
                TimelineRowKind::PendingMessage(_) => "pending",
                TimelineRowKind::Work { .. } => "work",
                TimelineRowKind::LiveWork { .. } => "live-work",
                TimelineRowKind::WorkToggle(_) => "work-toggle",
                TimelineRowKind::Thinking { .. } => "thinking",
                TimelineRowKind::Working => "working",
                TimelineRowKind::Fold(_) => "fold",
                TimelineRowKind::ContextCompaction { .. } => "compaction",
                TimelineRowKind::Lifecycle(_) => "lifecycle",
                TimelineRowKind::Subagents(_) => "subagents",
                TimelineRowKind::Handoff(_) => "handoff",
                TimelineRowKind::ProposedPlan(_) => "plan",
                TimelineRowKind::WorktreeSetup { .. } => "setup",
            };
            (row.id.clone(), kind)
        })
        .collect()
}

fn pending(id: &str, queued: bool, phase: Phase) -> PendingMessage {
    PendingMessage {
        command: CommandId::new(id).unwrap(),
        thread: thread_id(),
        id: MessageId::new(id).unwrap(),
        text: id.into(),
        attachments: vec![],
        context: None,
        queued,
        created_at: timestamp("2026-09-06T10:00:05Z"),
        phase,
    }
}

#[test]
fn a_settled_turn_folds_its_work_on_desktop_and_offers_edit_from_here() {
    let state = finished_state();
    let shell = agent_domain::shell(&state).unwrap();
    let sync = synced(state, 10);
    let thread = thread();
    let rows = timeline_rows(
        source(&thread, &sync, Some(&shell), &[]),
        &layout(TimelineLayout::Desktop),
    );
    assert_eq!(
        kinds(&rows),
        [
            ("message-user".into(), "user"),
            ("turn-fold:run-1".into(), "fold"),
            ("message-answer".into(), "assistant"),
        ]
    );
    let TimelineRowKind::UserMessage(user) = &rows[0].kind else {
        unreachable!()
    };
    let edit = user.decorations.edit_from_here.clone().unwrap();
    assert_eq!((edit.turn_count, edit.enabled), (0, true));
    let TimelineRowKind::Fold(fold) = &rows[1].kind else {
        unreachable!()
    };
    assert_eq!(fold.label, "Worked for 3.0s");
    let TimelineRowKind::AssistantMessage(answer) = &rows[2].kind else {
        unreachable!()
    };
    assert_eq!(answer.text, "All green.");
    assert!(answer.meta.as_ref().unwrap().copy.visible);
    let files = answer.changed_files.as_ref().unwrap();
    assert_eq!(files.title, "1 changed file");
}

#[test]
fn an_expanded_fold_shows_the_command_and_loads_its_withheld_output() {
    let state = finished_state();
    let shell = agent_domain::shell(&state).unwrap();
    let sync = synced(state, 10);
    let thread = thread();
    let mut options = layout(TimelineLayout::Desktop);
    options.expanded_runs.insert(RunId::new("run-1").unwrap());
    options.expanded_entries.insert("item-command".into());
    let rows = timeline_rows(source(&thread, &sync, Some(&shell), &[]), &options);
    let work = rows
        .iter()
        .find_map(|row| match &row.kind {
            TimelineRowKind::Work { rows, .. } => Some(rows),
            _ => None,
        })
        .expect("the command row");
    let WorkLogRow::Activity(command) = &work[0] else {
        panic!("a call row")
    };
    assert_eq!(command.label, "cargo test");
    assert!(command.expanded && command.load_detail);
}

#[test]
fn the_mobile_layout_groups_the_same_turn_into_feed_rows() {
    let state = finished_state();
    let shell = agent_domain::shell(&state).unwrap();
    let sync = synced(state, 10);
    let thread = thread();
    let rows = timeline_rows(
        source(&thread, &sync, Some(&shell), &[]),
        &layout(TimelineLayout::Mobile),
    );
    let ids: Vec<_> = kinds(&rows).into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(ids.first(), Some(&"user"));
    assert_eq!(ids.last(), Some(&"assistant"));
    assert!(ids.contains(&"fold"), "{ids:?}");
}

#[test]
fn unsent_messages_follow_the_timeline_and_queued_ones_stay_in_the_desktop_queue() {
    let state = finished_state();
    let shell = agent_domain::shell(&state).unwrap();
    let sync = synced(state, 10);
    let thread = thread();
    let unsent = [
        pending("sending", false, Phase::InFlight),
        pending("queued", true, Phase::Queued),
    ];
    let desktop = timeline_rows(
        source(&thread, &sync, Some(&shell), &unsent),
        &layout(TimelineLayout::Desktop),
    );
    let pending_rows: Vec<_> = kinds(&desktop)
        .into_iter()
        .filter(|(_, kind)| *kind == "pending")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(pending_rows, ["sending"]);
    // A send in flight counts as work: the working and thinking rows follow it.
    assert_eq!(
        kinds(&desktop)[3..]
            .iter()
            .map(|(_, kind)| *kind)
            .collect::<Vec<_>>(),
        ["pending", "working", "thinking"]
    );
    let mobile = timeline_rows(
        source(&thread, &sync, Some(&shell), &unsent),
        &layout(TimelineLayout::Mobile),
    );
    let tail: Vec<_> = kinds(&mobile)
        .into_iter()
        .filter(|(_, kind)| *kind == "pending")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(tail, ["sending", "queued"]);
}

#[test]
fn a_thread_without_state_shows_only_its_unsent_messages() {
    let sync = ThreadSync::default();
    let thread = thread();
    let unsent = [pending("first", false, Phase::Queued)];
    let rows = timeline_rows(
        source(&thread, &sync, None, &unsent),
        &layout(TimelineLayout::Desktop),
    );
    assert_eq!(kinds(&rows), [("first".into(), "pending")]);
}

#[test]
fn a_running_turn_shows_how_long_it_has_been_working() {
    let mut state = finished_state();
    state.items.push(
        user_message("item-next", "message-next")
            .run("run-2")
            .ordinal(4)
            .started("2026-09-06T10:01:00Z"),
    );
    let mut next = message("message-next", Role::User, "Again");
    next.run = Some(RunId::new("run-2").unwrap());
    state.messages.push(next);
    let mut second = run("run-2", 2, RunStatus::Running);
    second.started_at = Some(timestamp("2026-09-06T10:01:00Z"));
    state.runs.push(second);
    let mut shell = agent_domain::shell(&state).unwrap();
    shell.activity_run_started_at = Some(timestamp("2026-09-06T10:01:00Z"));
    let sync = synced(state, 11);
    let thread = thread();
    let rows = timeline_rows(
        source(&thread, &sync, Some(&shell), &[]),
        &layout(TimelineLayout::Desktop),
    );
    let working = rows
        .iter()
        .find(|row| row.kind == TimelineRowKind::Working)
        .expect("a working row");
    assert_eq!(working.created_at, Some(timestamp("2026-09-06T10:01:00Z")));
}

#[test]
fn the_cache_rebuilds_only_when_the_rows_inputs_change() {
    let state = finished_state();
    let shell = agent_domain::shell(&state).unwrap();
    let mut sync = synced(state, 10);
    let thread = thread();
    let options = layout(TimelineLayout::Desktop);
    let mut cache = TimelineCache::default();
    let first = cache.timeline(source(&thread, &sync, Some(&shell), &[]), &options);
    sync.status = ThreadStatus::Synchronizing;
    sync.error = Some("offline".into());
    let unchanged = cache.timeline(source(&thread, &sync, Some(&shell), &[]), &options);
    assert!(Arc::ptr_eq(&first.rows, &unchanged.rows));
    // A fact that leaves every row as it was keeps the revision.
    sync.cursor = 11;
    let same_rows = cache.timeline(source(&thread, &sync, Some(&shell), &[]), &options);
    assert_eq!(same_rows.revision, first.revision);
    let mut state = (*sync.state.clone().unwrap()).clone();
    state.messages[1].text = "All green, 12 passed.".into();
    sync.state = Some(Arc::new(state));
    sync.cursor = 12;
    let changed = cache.timeline(source(&thread, &sync, Some(&shell), &[]), &options);
    assert_eq!(changed.revision, first.revision + 1);
    let update = timeline_update(&first.rows, &changed.rows);
    assert!(update.splice.is_empty());
    assert_eq!(update.changed, [2]);
}

#[test]
fn a_loaded_detail_changes_the_row_key_and_shows_the_output() {
    let state = finished_state();
    let shell = agent_domain::shell(&state).unwrap();
    let mut sync = synced(state.clone(), 10);
    let thread = thread();
    let mut options = layout(TimelineLayout::Desktop);
    options.expanded_runs.insert(RunId::new("run-1").unwrap());
    options.expanded_entries.insert("item-command".into());
    let mut cache = TimelineCache::default();
    let before = cache.timeline(source(&thread, &sync, Some(&shell), &[]), &options);
    let loaded = state.items[1].clone().text("test result: ok");
    Arc::make_mut(&mut sync.details).insert(
        TurnItemId::new("item-command").unwrap(),
        Detail::Loaded(Box::new(loaded)),
    );
    sync.detail_revision += 1;
    let after = cache.timeline(source(&thread, &sync, Some(&shell), &[]), &options);
    assert_eq!(after.revision, before.revision + 1);
    let output = after.rows.iter().find_map(|row| match &row.kind {
        TimelineRowKind::Work { rows, .. } => match &rows[0] {
            WorkLogRow::Activity(row) => row.detail.as_ref()?.output.clone(),
            WorkLogRow::ProviderFailure(_) => None,
        },
        _ => None,
    });
    assert_eq!(output.as_deref(), Some("test result: ok"));
}
