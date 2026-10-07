use super::*;
use crate::{
    commands::outbox::{Delivered, Phase, Request},
    state::*,
    sync::{ThreadStatus, ThreadSync, fixtures::*},
};
use agent_domain::{
    Checkpoint, CheckpointId, CheckpointStatus, Command, FactBody, Message, MessageAuthor,
    MessageId, Reply, Role, RunStatus, ThreadId,
};
use agent_protocol::conversation::{
    Committed, ConversationError, ShellLocation, ShellSnapshot, ShellUpdate, ThreadUpdate,
};
use calls::{JobResult, Reply as CallReply};
use intents::Next;
use owner::{Network, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};

fn options() -> StoreOptions {
    StoreOptions {
        creation_source: "desktop".into(),
        cache_directory: None,
        state_file: None,
        start_on_list: false,
    }
}
fn owner(state: Snapshot) -> Owner {
    let (sender, _) = mpsc::channel(64);
    Owner::new(state, options(), sender).0
}
fn draft() -> Draft {
    Draft {
        instance_id: "codex".into(),
        model: "gpt".into(),
        ..Draft::default()
    }
}
fn row(id: &ThreadId) -> agent_domain::ThreadShell {
    let mut row = agent_domain::shell(&thread_state("Thread")).unwrap();
    row.id = id.clone();
    row
}
fn live_shell(owner: &mut Owner, sequence: u64, threads: Vec<agent_domain::ThreadShell>) {
    owner.shell_update(
        ShellLocation::Active,
        ShellUpdate::Snapshot(ShellSnapshot {
            snapshot_sequence: sequence,
            projects: vec![],
            threads,
        }),
    );
    owner.shell_update(ShellLocation::Active, ShellUpdate::Synchronized);
}
fn message(id: &str, text: &str) -> Message {
    Message {
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: None,
        role: Role::User,
        text: text.into(),
        attachments: vec![],
        intent: agent_domain::InputIntent::TurnStart,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        created_at: at(),
        updated_at: at(),
        context: None,
    }
}
/// An owner showing `thread` with its folded state.
fn opened(state: agent_domain::State) -> Owner {
    let mut owner = owner(Snapshot {
        default_draft: draft(),
        ..Snapshot::default()
    });
    owner.select_thread(Some(thread_id()));
    owner.thread_update(&thread_id(), snapshot(state, 1, None));
    owner.thread_update(&thread_id(), ThreadUpdate::Synchronized);
    owner
}
fn committed(sequence: u64, reply: Reply) -> Delivered {
    Delivered::Committed(Committed {
        reply,
        thread_sequence: sequence,
        sequence,
        replayed: false,
    })
}
fn commands(next: Next) -> Vec<crate::commands::outbox::PendingCommand> {
    let Next::Commands(entries) = next else {
        panic!("outbox commands")
    };
    entries
}

#[test]
fn a_send_clears_the_composer_shows_the_message_and_restores_it_when_refused() {
    let mut owner = opened(thread_state("Thread"));
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "Send me".into(),
            ..draft()
        },
    );
    let (sender, mut receipt) = oneshot::channel();
    owner.intent(Intent::Send { alternate: false }, sender);
    assert_eq!(owner.state.current_draft().text, "");
    let pending = owner
        .state
        .outbox
        .undelivered_messages(&thread_id(), owner.state.selected_state());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "Send me");
    let id = pending[0].command.clone();
    let Request::Dispatch(dispatch) = &owner.state.outbox.get(&id).unwrap().request else {
        panic!("dispatch")
    };
    let Command::Send(send) = &dispatch.command else {
        panic!("send")
    };
    // An idle thread starts a turn; the Host resolves the delivery.
    assert_eq!(send.intent, Some(agent_domain::DeliveryIntent::Auto));
    assert_eq!(send.title_seed.as_deref(), Some("Send me"));
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "typed meanwhile".into(),
            ..draft()
        },
    );
    owner.delivered(
        id,
        committed(
            2,
            Reply::Rejected {
                reason: "thread-archived".into(),
            },
        ),
    );
    assert_eq!(
        owner.state.current_draft().text,
        "typed meanwhile\n\nSend me"
    );
    assert!(owner.state.outbox.is_empty());
    assert!(receipt.try_recv().unwrap().is_err());
}

#[test]
fn a_running_thread_queues_follow_ups_and_the_alternate_steers() {
    let mut state = thread_state("Thread");
    state.runs.push(run("active", 1, RunStatus::Running));
    for (alternate, expected) in [
        (false, (agent_domain::DispatchMode::QueueAfterActive, None)),
        (
            true,
            (
                agent_domain::DispatchMode::StartImmediately,
                Some(agent_domain::DeliveryIntent::Steer),
            ),
        ),
    ] {
        let mut owner = opened(state.clone());
        owner.state.drafts.insert(
            thread_id().to_string(),
            Draft {
                text: "follow up".into(),
                ..draft()
            },
        );
        let entry = commands(owner.prepare(Intent::Send { alternate }).unwrap()).remove(0);
        let Request::Dispatch(dispatch) = entry.request else {
            panic!("dispatch")
        };
        let Command::Send(send) = dispatch.command else {
            panic!("send")
        };
        assert_eq!((send.mode, send.intent), expected);
    }
}

#[test]
fn a_launch_opens_its_thread_once_the_shell_shows_it() {
    let mut owner = owner(Snapshot {
        default_draft: Draft {
            text: "Fix the bug".into(),
            ..draft()
        },
        ..Snapshot::default()
    });
    let (sender, mut receipt) = oneshot::channel();
    owner.intent(Intent::Send { alternate: false }, sender);
    let entry = owner.state.outbox.entries[0].clone();
    let Request::Launch(launch) = &entry.request else {
        panic!("launch")
    };
    assert_eq!(launch.title, "Fix the bug");
    assert_eq!(launch.project_id, CHATS_PROJECT);
    assert_eq!(launch.thread_id.as_ref(), Some(&entry.thread));
    owner.state.drafts.insert(
        "new:chats".into(),
        Draft {
            text: "Next message".into(),
            ..draft()
        },
    );
    owner.delivered(entry.id.clone(), committed(5, Reply::Accepted));
    assert_eq!(owner.state.selected_thread, None);
    live_shell(&mut owner, 5, vec![row(&entry.thread)]);
    assert_eq!(owner.state.selected_thread, Some(entry.thread.clone()));
    assert_eq!(owner.state.current_draft().text, "Next message");
    assert!(owner.state.drafts["new:chats"].text.is_empty());
    assert_eq!(
        receipt.try_recv().unwrap().unwrap(),
        Outcome::StartedThread {
            id: entry.thread.to_string()
        }
    );
}

#[test]
fn a_late_launch_does_not_navigate_away_from_another_thread() {
    let mut owner = owner(Snapshot {
        default_draft: Draft {
            text: "Launch".into(),
            ..draft()
        },
        ..Snapshot::default()
    });
    owner.intent(Intent::Send { alternate: false }, oneshot::channel().0);
    let entry = owner.state.outbox.entries[0].clone();
    let other = ThreadId::new("other").unwrap();
    owner.select_thread(Some(other.clone()));
    owner.delivered(entry.id, committed(5, Reply::Accepted));
    live_shell(&mut owner, 5, vec![row(&entry.thread), row(&other)]);
    assert_eq!(owner.state.selected_thread, Some(other));
}

#[test]
fn lifecycle_previews_show_until_the_shell_confirms_them() {
    let mut owner = owner(Snapshot::default());
    live_shell(&mut owner, 1, vec![row(&thread_id())]);
    owner.intent(
        Intent::Thread {
            thread_id: thread_id().to_string(),
            action: ThreadAction::Pin,
        },
        oneshot::channel().0,
    );
    let shown = owner.state.shell_view().unwrap().threads[0].clone();
    assert!(shown.pinned_at.is_some());
    assert!(
        owner.state.shell.snapshot.as_ref().unwrap().threads[0]
            .pinned_at
            .is_none()
    );
    let id = owner.state.outbox.entries[0].id.clone();
    owner.delivered(id, committed(2, Reply::Accepted));
    let mut pinned = row(&thread_id());
    pinned.pinned_at = Some(at());
    owner.shell_update(
        ShellLocation::Active,
        ShellUpdate::ThreadUpdated {
            sequence: 2,
            thread: Box::new(pinned.clone()),
        },
    );
    assert!(owner.state.outbox.is_empty());
    assert_eq!(owner.state.shell_view().unwrap().threads[0], pinned);
}

fn now() -> i64 {
    owner::now_ms() as i64
}

fn confirm_last(owner: &mut Owner, sequence: u64) {
    let id = owner.state.outbox.entries.last().unwrap().id.clone();
    owner.delivered(id, committed(sequence, Reply::Accepted));
    owner.shell_update(
        ShellLocation::Active,
        ShellUpdate::ThreadUpdated {
            sequence,
            thread: Box::new(owner.state.thread_row(&thread_id()).unwrap().clone()),
        },
    );
}

fn lifecycle_commands(next: Next) -> Vec<Command> {
    commands(next)
        .into_iter()
        .map(|entry| match entry.request {
            Request::Dispatch(dispatch) => dispatch.command,
            Request::Launch(_) => panic!("dispatch"),
        })
        .collect()
}

// web useThreadActions.undo.test.ts "re-pins and re-snoozes a thread that settling had cleared"
#[test]
fn undoing_a_settle_restores_the_pin_and_snooze_it_cleared() {
    let mut owner = owner(Snapshot::default());
    let mut pinned = row(&thread_id());
    pinned.pinned_at = Some(at());
    pinned.pin_order = Some("a0".into());
    let until = agent_domain::Timestamp::parse("2030-01-01T09:00:00.000Z").unwrap();
    pinned.snoozed_until = Some(until.clone());
    live_shell(&mut owner, 1, vec![pinned]);
    owner.intent(
        Intent::Thread {
            thread_id: thread_id().to_string(),
            action: ThreadAction::Settle,
        },
        oneshot::channel().0,
    );
    assert_eq!(owner.state.thread_undo_notice(now()), None);
    confirm_last(&mut owner, 2);
    let notice = owner.state.thread_undo_notice(now()).unwrap();
    assert_eq!(notice.label, "Settled 1 thread");
    let restored = lifecycle_commands(owner.prepare(Intent::UndoThreadAction).unwrap());
    assert_eq!(
        restored,
        [
            Command::Settle {
                settled: false,
                at: None
            },
            Command::Pin {
                pinned: true,
                order: Some("a0".into())
            },
            Command::Snooze { until: Some(until) },
        ]
    );
    assert_eq!(owner.state.thread_undo_notice(now()), None);
}

// web useThreadActions.undo.test.ts "shows no Undo when the archive failed" and
// "unarchives and returns to the thread when archiving left it"
#[test]
fn an_archive_offers_undo_only_once_it_went_through() {
    for accepted in [false, true] {
        let mut owner = owner(Snapshot::default());
        live_shell(&mut owner, 1, vec![row(&thread_id())]);
        owner.select_thread(Some(thread_id()));
        owner.intent(
            Intent::Thread {
                thread_id: thread_id().to_string(),
                action: ThreadAction::Archive,
            },
            oneshot::channel().0,
        );
        let entry = owner.state.outbox.entries[0].clone();
        if !accepted {
            owner.fail(entry, "nope".into());
            assert_eq!(owner.state.thread_undo_notice(now()), None);
            continue;
        }
        confirm_last(&mut owner, 2);
        assert_eq!(
            owner.state.thread_undo_notice(now()).unwrap().action,
            crate::commands::undo::ThreadUndoAction::Archived
        );
        owner.select_thread(None);
        let restored = lifecycle_commands(owner.prepare(Intent::UndoThreadAction).unwrap());
        assert_eq!(restored, [Command::Archive { archived: false }]);
        assert_eq!(owner.state.selected_thread, Some(thread_id()));
    }
}

// web useThreadActions.undo.test.ts "expires an older unpin Undo when the thread is settled"
#[test]
fn settling_expires_an_older_unpin_undo() {
    let mut owner = owner(Snapshot::default());
    let mut pinned = row(&thread_id());
    pinned.pinned_at = Some(at());
    live_shell(&mut owner, 1, vec![pinned]);
    for (action, sequence) in [(ThreadAction::Unpin, 2), (ThreadAction::Settle, 3)] {
        owner.intent(
            Intent::Thread {
                thread_id: thread_id().to_string(),
                action,
            },
            oneshot::channel().0,
        );
        confirm_last(&mut owner, sequence);
    }
    let notice = owner.state.thread_undo_notice(now()).unwrap();
    assert_eq!(notice.label, "Settled 1 thread");
    owner.prepare(Intent::UndoThreadAction).unwrap();
    assert_eq!(owner.state.thread_undo_notice(now()), None);
}

// web discardComposerDraft.test.ts
#[test]
fn a_discarded_draft_comes_back_unless_new_text_replaced_it() {
    for typed_again in [false, true] {
        let mut owner = owner(Snapshot::default());
        let draft = Draft {
            text: "half-written prompt".into(),
            ..Draft::default()
        };
        owner.state.drafts.insert("new:app".into(), draft.clone());
        owner
            .prepare(Intent::DiscardDraft {
                draft_key: "new:app".into(),
            })
            .unwrap();
        assert!(!owner.state.drafts.contains_key("new:app"));
        assert_eq!(
            owner.state.thread_undo_notice(now()).unwrap().label,
            "Discarded 1 draft"
        );
        if typed_again {
            let newer = Draft {
                text: "new reply".into(),
                ..Draft::default()
            };
            owner.state.drafts.insert("new:app".into(), newer.clone());
            assert!(owner.prepare(Intent::UndoThreadAction).is_err());
            assert_eq!(owner.state.drafts["new:app"], newer);
        } else {
            owner.prepare(Intent::UndoThreadAction).unwrap();
            assert_eq!(owner.state.drafts["new:app"], draft);
        }
    }
}

#[test]
fn a_drop_on_another_section_changes_the_threads_state_with_its_key() {
    use crate::view::thread_order::{DropSection, MoveDestination, OrderSection, Placement};
    let mut owner = owner(Snapshot::default());
    let other = ThreadId::new("other").unwrap();
    live_shell(&mut owner, 1, vec![row(&thread_id()), row(&other)]);
    let drop_on = |section| MoveDestination::Drop {
        target: None,
        section: Some(section),
        placement: Placement::Before,
    };
    owner.intent(
        Intent::MoveThread {
            thread_id: thread_id().to_string(),
            section: OrderSection::Active,
            destination: drop_on(DropSection::Pinned),
        },
        oneshot::channel().0,
    );
    let shown = owner.state.shell_view().unwrap();
    let pinned = shown.threads.iter().find(|row| row.id == thread_id()).unwrap();
    assert!(pinned.pinned_at.is_some());
    assert!(owner.state.thread_order.is_none());
    let pending = owner.state.outbox.entries.len();
    owner.intent(
        Intent::MoveThread {
            thread_id: other.to_string(),
            section: OrderSection::Active,
            destination: drop_on(DropSection::Settled),
        },
        oneshot::channel().0,
    );
    assert_eq!(owner.state.outbox.entries.len(), pending + 1);
}

#[test]
fn a_rollback_returns_the_rolled_back_message_to_the_composer_after_success_only() {
    for succeeded in [true, false] {
        let mut state = thread_state("Thread");
        let mut first = run("first", 1, RunStatus::Completed);
        first.message = MessageId::new("first-message").unwrap();
        state.runs.push(first);
        state.messages.push(message("first-message", "preparing"));
        state.checkpoints.push(Checkpoint {
            status: CheckpointStatus::Ready,
            scope: None,
            id: CheckpointId::new("start").unwrap(),
            run: None,
            run_ordinal: 0,
            native_heads: BTreeMap::new(),
            file_ref: "refs/start".into(),
            files: vec![],
        });
        let mut owner = opened(state);
        owner.state.drafts.insert(
            thread_id().to_string(),
            Draft {
                text: "unsent".into(),
                ..draft()
            },
        );
        let entry = commands(
            owner
                .prepare(Intent::Rollback {
                    checkpoint_id: "start".into(),
                    restore_files: false,
                })
                .unwrap(),
        )
        .remove(0);
        let id = entry.id.clone();
        owner.enqueue(entry, None).unwrap();
        owner.delivered(id.clone(), committed(2, Reply::Accepted));
        assert_eq!(owner.state.current_draft().text, "unsent");
        owner.thread_update(
            &thread_id(),
            facts(vec![(
                2,
                fact(FactBody::RollbackRequested {
                    command: id.clone(),
                    checkpoint: CheckpointId::new("start").unwrap(),
                    restore_files: false,
                    after_start: None,
                }),
            )]),
        );
        let outcome = if succeeded {
            FactBody::RolledBack {
                command: id.clone(),
                checkpoint: CheckpointId::new("start").unwrap(),
            }
        } else {
            FactBody::RollbackFailed {
                command: id.clone(),
                message: "failed".into(),
            }
        };
        owner.thread_update(&thread_id(), facts(vec![(3, fact(outcome))]));
        assert_eq!(
            owner.state.current_draft().text,
            if succeeded {
                "unsent\n\npreparing"
            } else {
                "unsent"
            }
        );
        assert!(owner.state.rollbacks.is_empty());
    }
}

#[test]
fn a_queued_edit_ends_when_its_run_leaves_the_queue_and_restores_the_main_draft() {
    let mut state = thread_state("Thread");
    state.runs.push(run("active", 1, RunStatus::Running));
    let mut queued = run("queued", 2, RunStatus::Queued);
    queued.message = MessageId::new("queued-message").unwrap();
    state.runs.push(queued.clone());
    state
        .messages
        .push(message("queued-message", "queued text"));
    let mut owner = opened(state);
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "keep my draft".into(),
            ..draft()
        },
    );
    owner
        .prepare(Intent::Queue {
            action: QueueAction::Edit {
                run_id: "queued".into(),
            },
        })
        .unwrap();
    assert_eq!(owner.state.current_draft().text, "queued text");
    owner.thread_update(
        &thread_id(),
        facts(vec![(
            2,
            fact(FactBody::RunStarted {
                checkpoint_scope: None,
                native_baseline_heads: BTreeMap::new(),
                id: queued.id.clone(),
            }),
        )]),
    );
    assert!(owner.state.editing_run.is_none());
    assert_eq!(owner.state.current_draft().text, "keep my draft");
}

#[test]
fn a_cold_missing_thread_is_deleted_and_deselected() {
    let mut owner = owner(Snapshot::default());
    owner.select_thread(Some(thread_id()));
    owner.state.drafts.insert(thread_id().to_string(), draft());
    let gone = owner.thread_update(
        &thread_id(),
        ThreadUpdate::Failed(ConversationError::ThreadNotFound(thread_id()).into()),
    );
    assert_eq!(gone, Some(true));
    assert_eq!(owner.state.selected_thread, None);
    assert_eq!(
        owner.state.threads[&thread_id()].status,
        ThreadStatus::Deleted
    );
    assert!(!owner.state.drafts.contains_key(thread_id().as_str()));
}

#[test]
fn a_reopened_thread_resumes_from_its_retained_state_until_the_retention_ends() {
    let mut owner = opened(thread_state("Retained"));
    owner.select_thread(None);
    assert_eq!(owner.state.threads[&thread_id()].status, ThreadStatus::Live);
    owner.select_thread(Some(thread_id()));
    let retained = &owner.state.threads[&thread_id()];
    assert_eq!(retained.status, ThreadStatus::Live);
    assert_eq!(title(retained.state.as_ref().unwrap()), "Retained");
    owner.select_thread(None);
    owner.last_used.insert(thread_id(), 0);
    owner.tick();
    assert!(!owner.state.threads.contains_key(&thread_id()));
}

#[test]
fn a_reopened_thread_shows_its_disk_cache_and_resumes_from_its_cursor() {
    let directory = tempfile::tempdir().unwrap();
    let cache = crate::sync::DiskCache::new(directory.path());
    cache
        .save_thread(
            &thread_id(),
            &crate::sync::CachedThread {
                snapshot_sequence: 7,
                state: std::sync::Arc::new(thread_state("Cached")),
                history_cursor: None,
                has_more_history: false,
                latest_local_ordinal: None,
            },
        )
        .unwrap();
    let (sender, _) = mpsc::channel(8);
    let mut owner = Owner::new(
        Snapshot::default(),
        StoreOptions {
            cache_directory: Some(directory.path().into()),
            ..options()
        },
        sender,
    )
    .0;
    owner.select_thread(Some(thread_id()));
    let sync = &owner.state.threads[&thread_id()];
    assert_eq!(sync.cursor, 7);
    assert_eq!(title(sync.state.as_ref().unwrap()), "Cached");
    let mut resumed = ThreadSync::clone(sync);
    assert_eq!(
        resumed.subscribe(&thread_id()).unwrap().after_sequence,
        Some(7)
    );
}

#[test]
fn stop_targets_the_active_run_and_holds_the_queue() {
    let mut state = thread_state("Thread");
    state.runs.push(run("active", 1, RunStatus::Running));
    let mut owner = opened(state);
    let entry = commands(owner.prepare(Intent::Stop).unwrap()).remove(0);
    let Request::Dispatch(dispatch) = entry.request else {
        panic!("dispatch")
    };
    assert_eq!(
        dispatch.command,
        Command::Interrupt {
            run: agent_domain::RunId::new("active").unwrap(),
            hold_queue: true,
            reason: None,
        }
    );
    let mut idle = opened(thread_state("Thread"));
    assert!(idle.prepare(Intent::Stop).is_err());
}

#[test]
fn search_respects_server_limits_and_clear_remains_local() {
    let mut owner = owner(Snapshot::default());
    owner.state.connected = true;
    owner.network = None;
    assert!(matches!(
        owner
            .prepare(Intent::Search {
                query: "bug".into()
            })
            .unwrap(),
        Next::Done
    ));
    for query in ["", "a"] {
        assert!(matches!(
            owner
                .prepare(Intent::Search {
                    query: query.into()
                })
                .unwrap(),
            Next::Done
        ));
    }
    assert_eq!(owner.state.search, "a");
    assert_eq!(owner.state.search_request, None);
}

// web queries.ts useThreadSearch: a query is asked once typing settles, is
// pending until its own answer arrives, and a stale answer is dropped.
#[test]
fn a_message_search_is_pending_until_its_own_answer_arrives() {
    use crate::state::SearchRequest;
    let mut owner = owner(Snapshot::default());
    owner.state.connected = true;
    owner.state.search = "needle".into();
    owner.state.search_request = Some(SearchRequest {
        query: "needle".into(),
        due_at_ms: Some(10),
    });
    assert_eq!(owner.sources_deadline(), Some(10));
    owner.search_finished("needle", Some(vec![]));
    assert!(owner.state.search_request.is_some(), "not asked yet");
    owner.sources_tick(9);
    assert_eq!(
        owner.state.search_request.as_ref().unwrap().due_at_ms,
        Some(10)
    );
    owner.sources_tick(10);
    assert_eq!(owner.state.search_request.as_ref().unwrap().due_at_ms, None);
    assert!(crate::view::search::search_pending(&owner.state));
    let found = agent_protocol::conversation::SearchMatch {
        thread_id: thread_id(),
        project_id: "project".into(),
        source: agent_protocol::conversation::SearchSource::User,
        snippet: "needle".into(),
        message_created_at: None,
    };
    owner.search_finished("needl", Some(vec![found.clone()]));
    assert!(owner.state.search_matches.is_empty());
    owner.search_finished("needle", Some(vec![found]));
    assert_eq!(owner.state.search_matches.len(), 1);
    assert!(!crate::view::search::search_pending(&owner.state));
}

#[test]
fn text_edits_preserve_newer_model_and_mode_choices() {
    let mut owner = opened(thread_state("Thread"));
    let chosen = Draft {
        model: "chosen model".into(),
        runtime_mode: agent_domain::RuntimeMode::ApprovalRequired,
        text: "before".into(),
        ..draft()
    };
    owner
        .state
        .drafts
        .insert(thread_id().to_string(), chosen.clone());
    owner
        .prepare(Intent::EditDraft {
            text: "after".into(),
            base_text: Some("before".into()),
        })
        .unwrap();
    assert_eq!(
        owner.state.current_draft(),
        Draft {
            text: "after".into(),
            ..chosen
        }
    );
}

#[test]
fn stopping_retries_returns_the_unsent_message_to_the_composer() {
    let mut owner = opened(thread_state("Thread"));
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "unsent".into(),
            ..draft()
        },
    );
    owner.intent(Intent::Send { alternate: false }, oneshot::channel().0);
    let id = owner.state.outbox.entries[0].id.clone();
    owner.state_outbox().sending(&id);
    owner.delivered(
        id.clone(),
        Delivered::Unknown("receipt decode failed".into()),
    );
    assert!(matches!(
        owner.state.outbox.entries[0].phase,
        Phase::Uncertain { .. }
    ));
    owner
        .prepare(Intent::DiscardPending {
            command_id: id.to_string(),
        })
        .unwrap();
    assert!(owner.state.outbox.is_empty());
    assert_eq!(owner.state.current_draft().text, "unsent");
}

#[test]
fn failed_background_visits_do_not_replace_a_user_notice() {
    let mut owner = opened(thread_state("Thread"));
    owner.state.error = Some("user notice".into());
    let entry = owner.pending(thread_id(), Command::Visit { at: at() });
    let id = entry.id.clone();
    owner.enqueue(entry, None).unwrap();
    owner.delivered(
        id,
        committed(
            2,
            Reply::Rejected {
                reason: "background failure".into(),
            },
        ),
    );
    assert_eq!(owner.state.error.as_deref(), Some("user notice"));
}

#[tokio::test]
async fn mobile_cold_start_keeps_drafts_without_reopening_the_saved_selection() {
    let mut state = Snapshot {
        selected_thread: Some(thread_id()),
        ..Snapshot::default()
    };
    state.drafts.insert(
        "thread".into(),
        Draft {
            text: "keep me".into(),
            ..Draft::default()
        },
    );
    let store = Store::offline(
        state,
        StoreOptions {
            start_on_list: true,
            ..options()
        },
    );
    assert!(store.snapshot().selected_thread.is_none());
    assert_eq!(store.snapshot().drafts["thread"].text, "keep me");
    store.close().await.unwrap();
}

#[tokio::test]
async fn snapshot_revisions_are_ordered_within_each_store_only() {
    let first = Store::offline(
        Snapshot {
            revision: 4000,
            ..Default::default()
        },
        options(),
    );
    let second = Store::offline(Snapshot::default(), options());
    assert!(second.snapshot().accepts_after(&first.snapshot()));
    let mut old = (*second.snapshot()).clone();
    old.revision = 4;
    let mut next = old.clone();
    next.revision = 5;
    assert!(!old.accepts_after(&next));
    assert!(next.accepts_after(&old));
    first.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn a_burst_of_input_over_the_stream_channel_capacity_keeps_every_edit() {
    let store = Store::offline(Snapshot::default(), options());
    let mut receipts = vec![];
    for i in 0..200 {
        receipts.push(store.dispatch(Intent::EditDraft {
            base_text: None,
            text: i.to_string(),
        }));
    }
    for receipt in receipts {
        receipt.await.unwrap().unwrap();
    }
    assert_eq!(store.snapshot().current_draft().text, "199");
    store.close().await.unwrap();
}

#[tokio::test]
async fn shutdown_closes_snapshot_waiters_and_preserves_local_edits() {
    let store = Store::offline(Snapshot::default(), options());
    store
        .dispatch(Intent::EditDraft {
            base_text: None,
            text: "Unsent".into(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.snapshot().current_draft().text, "Unsent");
    let mut snapshots = store.subscribe();
    store.close().await.unwrap();
    while snapshots.changed().await.is_ok() {}
    assert!(!snapshots.borrow().connected);
}

#[test]
fn dictation_preparation_uses_the_lossless_intent_queue_when_stream_events_are_full() {
    let (sender, _receiver) = mpsc::channel(1);
    sender
        .try_send(Event::Performance(Default::default()))
        .unwrap_or_else(|_| panic!("empty channel"));
    let (intents, mut input) = mpsc::unbounded_channel();
    let (_, snapshots) = watch::channel(Arc::new(Snapshot::default()));
    let store = Store {
        inner: Arc::new(Inner {
            sender,
            intents,
            snapshots,
            stop: CancellationToken::new(),
        }),
    };
    let preparation = store.prepare_dictation();
    let Event::Dictation(id, token) = input.try_recv().unwrap() else {
        panic!("preparation")
    };
    assert_eq!(id, preparation.id());
    assert!(!token.is_cancelled());
    drop(preparation);
    assert!(token.is_cancelled());
}

fn job(
    call: Call,
    result: Result<CallReply, PeerError>,
    sent: Option<(String, Draft)>,
) -> JobResult {
    JobResult {
        call,
        result,
        complete: None,
        sent,
    }
}

#[test]
fn a_cancelled_dictation_cannot_append_a_late_transcript() {
    let mut owner = opened(thread_state("Thread"));
    let key = owner.state.draft_key();
    owner.state.drafts.insert(
        key.clone(),
        Draft {
            text: "keep".into(),
            ..draft()
        },
    );
    let token = CancellationToken::new();
    owner.dictations.insert("recording".into(), token.clone());
    token.cancel();
    owner.finished(job(
        Call::Transcribe(op::Transcribe {
            preparation: Some("recording".into()),
            audio: vec![],
        }),
        Ok(CallReply::Transcription("must not append".into())),
        Some((key, owner.state.current_draft())),
    ));
    assert_eq!(owner.state.current_draft().text, "keep");
    assert!(owner.state.error.is_none());
    assert!(owner.dictations.is_empty());
}

#[test]
fn a_transcription_appends_to_its_draft_without_replacing_new_text() {
    let mut owner = owner(Snapshot::default());
    owner.state.drafts.insert(
        "thread".into(),
        Draft {
            text: "Typed while recording".into(),
            ..Draft::default()
        },
    );
    owner.finished(job(
        Call::Transcribe(op::Transcribe {
            audio: vec![],
            preparation: None,
        }),
        Ok(CallReply::Transcription("Dictated words".into())),
        Some(("thread".into(), Draft::default())),
    ));
    assert_eq!(
        owner.state.drafts["thread"].text,
        "Typed while recording\nDictated words"
    );
}

#[test]
fn a_remote_pairing_receipt_observes_the_registered_host() {
    let mut owner = owner(Snapshot::default());
    let (complete, mut receipt) = oneshot::channel();
    owner.finished(JobResult {
        call: Call::RegisterRemote(op::RegisterRemoteHost {
            ticket: "ticket".into(),
            name: "Host".into(),
        }),
        result: Ok(CallReply::Remote(m::RemoteHost {
            id: "remote".into(),
            ticket: "ticket".into(),
            name: "Host".into(),
        })),
        complete: Some(complete),
        sent: None,
    });
    assert_eq!(
        receipt.try_recv().unwrap().unwrap(),
        Outcome::RemoteHostPaired {
            id: "remote".into()
        }
    );
    assert_eq!(owner.state.remote_hosts[0].id, "remote");
}

#[test]
fn a_late_turn_diff_cannot_replace_another_range_or_thread() {
    let mut owner = opened(thread_state("Thread"));
    let request = |owner: &mut Owner, from, to| {
        let Next::Call(call, _) = owner
            .prepare(Intent::ReadTurnDiff {
                from_run_ordinal: from,
                to_run_ordinal: to,
                ignore_whitespace: false,
            })
            .unwrap()
        else {
            panic!("diff request")
        };
        *call
    };
    let reply = |from, to| {
        Ok(CallReply::TurnDiff(
            agent_protocol::conversation::TurnDiff {
                thread_id: thread_id(),
                from_run_ordinal: from,
                to_run_ordinal: to,
                diff: String::new(),
            },
        ))
    };
    let first = request(&mut owner, 0, 1);
    let second = request(&mut owner, 1, 2);
    owner.finished(job(first, reply(0, 1), None));
    assert!(owner.state.workspace.review.is_none());
    owner.finished(job(second.clone(), reply(1, 2), None));
    assert_eq!(
        owner.state.workspace.review.as_ref().unwrap().branch,
        "Turns 1–2"
    );
    owner
        .prepare(Intent::NewThread { project_id: None })
        .unwrap();
    owner.finished(job(second, reply(1, 2), None));
    assert!(owner.state.workspace.review.is_none());
}

#[test]
fn file_reloads_and_saves_preserve_edits_and_their_base_revision() {
    let file = m::FileContent {
        path: "/file".into(),
        revision: "v1".into(),
        text: "original".into(),
        size: 8,
    };
    let mut owner = owner(Snapshot::default());
    owner.state.workspace.file = Some(Arc::new(file.clone()));
    owner
        .prepare(Intent::EditFile {
            path: file.path.clone(),
            text: "edit".into(),
        })
        .unwrap();
    let mut updated = file.clone();
    updated.revision = "external".into();
    owner.finished(job(
        Call::ReadFile(op::ListFiles {
            path: file.path.clone(),
        }),
        Ok(CallReply::File(updated)),
        None,
    ));
    let Next::Call(call, _) = owner
        .prepare(Intent::SaveFile {
            path: file.path.clone(),
        })
        .unwrap()
    else {
        panic!("expected file write")
    };
    let Call::WriteFile(written) = *call else {
        panic!("expected file write")
    };
    assert_eq!(written.revision, "v1");
    owner
        .prepare(Intent::EditFile {
            path: file.path.clone(),
            text: "typed during save".into(),
        })
        .unwrap();
    let mut saved = file.clone();
    saved.revision = "v2".into();
    saved.text = written.text.clone();
    owner.finished(job(
        Call::WriteFile(written),
        Ok(CallReply::File(saved)),
        None,
    ));
    let draft = &owner.state.workspace.file_drafts[&file.path];
    assert_eq!(draft.text, "typed during save");
    assert_eq!(draft.revision, "v2");
}

#[test]
fn a_failed_terminal_start_is_terminal_and_terminal_output_stays_bounded() {
    let mut owner = owner(Snapshot::default());
    let thread = ThreadId::new("thread").unwrap();
    let row = row(&thread);
    owner.shell_update(
        ShellLocation::Active,
        ShellUpdate::Snapshot(ShellSnapshot {
            snapshot_sequence: 1,
            projects: vec![crate::models::Project {
                id: row.project.clone(),
                roots: vec![crate::models::ProjectRoot {
                    path: "/tmp".into(),
                }],
                ..Default::default()
            }],
            threads: vec![row],
        }),
    );
    let Next::Call(call, _) = owner
        .prepare(Intent::OpenTerminal {
            thread_id: thread.to_string(),
            terminal_id: "term-1".into(),
            cols: 80,
            rows: 24,
        })
        .unwrap()
    else {
        panic!("terminal start")
    };
    let terminal = op::thread_terminal_handle_for("thread", "term-1");
    let terminal = terminal.as_str();
    let call = *call;
    for _ in 0..10 {
        owner.terminal_output(terminal, vec![1; 1024 * 1024], None);
    }
    let old = owner.state.clone();
    assert_eq!(old.terminals[terminal].output_bytes, 8 * 1024 * 1024);
    assert!(
        old.terminals[terminal]
            .output
            .shares_storage(&owner.state.terminals[terminal].output)
    );
    assert!(old.drafts.shares_storage(&owner.state.drafts));
    owner.terminal_output(
        terminal,
        b"reset".to_vec(),
        Some(op::TerminalSize { cols: 80, rows: 24 }),
    );
    assert_eq!(owner.state.terminals[terminal].output_bytes, 5);
    assert_eq!(old.terminals[terminal].output.len(), 8);
    owner.finished(job(
        call,
        Err(PeerError::ConnectionClosed("lost".into())),
        None,
    ));
    assert!(matches!(
        owner.state.terminals[terminal].phase,
        TerminalPhase::Failed(_)
    ));
}

/// An owner connected over loopback to a Host side the test answers by hand.
struct Loopback {
    owner: Owner,
    events: mpsc::Receiver<Event>,
    host: crate::transport::Session,
    _host_peer: agent_transport::client::HostPeer,
    session: crate::transport::Session,
    ticket: crate::transport::Ticket,
    endpoint: crate::transport::Endpoint,
    host_endpoint: crate::transport::Endpoint,
    client: Peer,
}
impl Loopback {
    /// `prepare` runs before the owner counts as connected, so it opens no streams.
    async fn connect(
        timeout: std::time::Duration,
        options: StoreOptions,
        prepare: impl FnOnce(&mut Owner),
    ) -> Self {
        use crate::transport::{Endpoint, Identity, Relays, Trust};
        let host_endpoint = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let endpoint = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let ticket = host_endpoint.local_ticket();
        let (outgoing, incoming) = tokio::join!(endpoint.connect(&ticket), async {
            host_endpoint.accept().await.unwrap().establish().await
        });
        let session = outgoing.unwrap();
        let host = incoming
            .unwrap()
            .authorize(&Trust {
                allowed: BTreeSet::from([endpoint.node_id()]),
                ..Default::default()
            })
            .unwrap();
        let (client, _events) = session.open_peer(timeout, 8).await.unwrap();
        let host_peer = host.accept_peer().await.unwrap();
        let client = Arc::new(client);
        let (sender, events) = mpsc::channel(8);
        let mut owner = Owner::new(Snapshot::default(), options, sender).0;
        prepare(&mut owner);
        owner.state.connected = true;
        owner.network = Some(Network {
            peer: client.clone(),
            session: session.clone(),
            ticket: ticket.clone(),
            epoch: 0,
            tasks: vec![],
            streams: BTreeMap::new(),
            deliveries: BTreeMap::new(),
        });
        Self {
            owner,
            events,
            host,
            _host_peer: host_peer,
            session,
            ticket,
            endpoint,
            host_endpoint,
            client,
        }
    }

    async fn next_call(&self) -> Box<agent_transport::client::HostRequest> {
        let crate::transport::IncomingRequest::Call(request) = self
            .host
            .accept_stream()
            .await
            .unwrap()
            .decode()
            .await
            .unwrap()
        else {
            panic!("a request")
        };
        request
    }

    async fn close(self) {
        drop(self.owner);
        self.session.close();
        self.endpoint.close().await;
        self.host_endpoint.close().await;
    }
}

/// Answers with the result type the Host's contract for the call declares.
async fn answer<C: crate::protocol::contracts::Contract>(
    request: &mut agent_transport::client::HostRequest,
    result: C::Output,
) where
    C::Output: serde::Serialize,
{
    agent_transport::framing::write(
        &mut request.send,
        crate::protocol::Response::Success { result },
    )
    .await
    .unwrap();
    request.send.finish().unwrap();
}

#[tokio::test]
async fn a_healthy_resume_reuses_the_connection_and_timed_out_sends_keep_their_id_and_order() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut host = Loopback::connect(std::time::Duration::from_millis(150), options(), |_| {}).await;
        let (complete, timed_out) = oneshot::channel();
        host.owner
            .handle(Event::Resume {
                endpoint: host.endpoint.clone(),
                ticket: host.ticket.clone(),
                complete,
            })
            .await;
        let unanswered = host.next_call().await;
        assert!(matches!(unanswered.call, Call::HostStatus(_)));
        assert!(
            timed_out.await.unwrap().is_none(),
            "open transport flags do not prove liveness"
        );
        drop(unanswered);
        let (complete, reply) = oneshot::channel();
        host.owner
            .handle(Event::Resume {
                endpoint: host.endpoint.clone(),
                ticket: host.ticket.clone(),
                complete,
            })
            .await;
        let mut probe = host.next_call().await;
        assert!(matches!(probe.call, Call::HostStatus(_)));
        answer::<crate::protocol::contracts::HostStatus>(
            &mut probe,
            m::HostStatus {
                name: "fixture".into(),
                node_id: "node".into(),
                devices: vec![],
                provider_errors: None,
            },
        )
        .await;
        let reused = reply.await.unwrap().unwrap();
        assert!(reused.reused);
        assert_eq!(reused.connection_id, host.client.diagnostic_id);

        live_shell(&mut host.owner, 1, vec![row(&thread_id())]);
        let first = host.owner.pending(
            thread_id(),
            Command::Pin {
                pinned: true,
                order: None,
            },
        );
        let second = host.owner.pending(
            thread_id(),
            Command::Pin {
                pinned: false,
                order: None,
            },
        );
        host.owner.enqueue(first.clone(), None).unwrap();
        host.owner.enqueue(second.clone(), None).unwrap();
        // The second request waits behind the first.
        assert_eq!(host.owner.state.outbox.entries[1].phase, Phase::Queued);
        let mut held = vec![];
        let expected = [first.id.clone(), first.id.clone(), second.id.clone()];
        for (index, expected) in expected.into_iter().enumerate() {
            let mut request = host.next_call().await;
            let Call::Dispatch(dispatch) = &request.call else {
                panic!("dispatch")
            };
            assert_eq!(dispatch.command_id, expected);
            if index == 0 {
                // Keep the response stream open until the client times out.
                held.push(request);
                continue;
            }
            answer::<crate::protocol::contracts::Dispatch>(
                &mut request,
                Committed {
                    reply: Reply::Accepted,
                    thread_sequence: index as u64,
                    sequence: index as u64,
                    replayed: index == 1,
                },
            )
            .await;
            let event = host.events.recv().await.unwrap();
            assert!(matches!(
                &event,
                Event::Delivered {
                    delivered: Delivered::Committed(_),
                    ..
                }
            ));
            host.owner.handle(event).await;
        }
        assert!(
            host.owner
                .state
                .outbox
                .entries
                .iter()
                .all(|entry| matches!(entry.phase, Phase::Committed { .. }))
        );
        host.owner.shell_update(
            ShellLocation::Active,
            ShellUpdate::ThreadUpdated {
                sequence: 2,
                thread: Box::new(row(&thread_id())),
            },
        );
        assert!(host.owner.state.outbox.is_empty());
        host.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn an_expanded_item_shows_the_withheld_output_the_host_reads() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut state = thread_state("Thread");
        let mut item = command_item("command", 1);
        item.output_omitted = true;
        state.items.push(item.clone());
        let mut host = Loopback::connect(std::time::Duration::from_secs(5), options(), |owner| {
            owner.select_thread(Some(thread_id()));
            owner.thread_update(&thread_id(), snapshot(state, 1, None));
        })
        .await;
        host.owner.load_detail(item.id.clone()).unwrap();
        let mut request = host.next_call().await;
        let Call::TurnItem(read) = &request.call else {
            panic!("item read")
        };
        assert_eq!(read.item_id, item.id);
        let mut full = item.clone();
        full.output_omitted = false;
        full.text = "the complete output".into();
        answer::<crate::protocol::contracts::TurnItem>(
            &mut request,
            Some(agent_protocol::conversation::TurnItemDetail {
                row: agent_protocol::conversation::HistoryRow {
                    position: 0,
                    source: thread_id(),
                    inherited: false,
                    item: full.clone(),
                    message: None,
                    plan: None,
                },
                task: None,
            }),
        )
        .await;
        let event = host.events.recv().await.unwrap();
        host.owner.handle(event).await;
        assert_eq!(
            host.owner.state.threads[&thread_id()].details.get(&item.id),
            Some(&crate::sync::Detail::Loaded(Box::new(full)))
        );
        host.close().await;
    })
    .await
    .unwrap();
}

#[test]
fn an_item_expanded_offline_is_not_left_loading() {
    let mut state = thread_state("Thread");
    let mut item = command_item("command", 1);
    item.output_omitted = true;
    state.items.push(item.clone());
    let mut owner = opened(state);
    assert!(owner.load_detail(item.id.clone()).is_err());
    assert_eq!(
        owner.state.threads[&thread_id()].details.get(&item.id),
        None
    );
}

#[test]
fn a_committed_refusal_reads_as_a_sentence_and_a_transport_failure_keeps_its_text() {
    let mut owner = opened(thread_state("Thread"));
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "Send me".into(),
            ..draft()
        },
    );
    let entry = commands(owner.prepare(Intent::Send { alternate: false }).unwrap()).remove(0);
    let id = entry.id.clone();
    owner.enqueue(entry, None).unwrap();
    owner.delivered(
        id,
        committed(
            2,
            Reply::Rejected {
                reason: "thread-archived".into(),
            },
        ),
    );
    let worded = crate::view::rejection::provider_rejection_message(
        "thread-archived",
        agent_domain::Driver::Codex,
    );
    assert_ne!(worded, "thread-archived");
    assert_eq!(owner.state.error.as_deref(), Some(worded.as_str()));

    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "Again".into(),
            ..draft()
        },
    );
    let entry = commands(owner.prepare(Intent::Send { alternate: false }).unwrap()).remove(0);
    let id = entry.id.clone();
    owner.enqueue(entry, None).unwrap();
    owner.delivered(
        id,
        Delivered::NotSent(agent_protocol::error::RpcFailure {
            code: "invalid_command".into(),
            message: "thread-archived".into(),
            delivery: agent_protocol::error::Delivery::NotSent,
        }),
    );
    assert_eq!(owner.state.error.as_deref(), Some("thread-archived"));
}

#[test]
fn moving_a_queued_message_sends_one_reorder() {
    let mut state = thread_state("Thread");
    state.runs.push(run("active", 1, RunStatus::Running));
    let mut first = run("first", 2, RunStatus::Queued);
    first.queue_position = Some(1);
    let mut second = run("second", 3, RunStatus::Queued);
    second.queue_position = Some(2);
    state.runs.extend([first, second]);
    let mut owner = opened(state);
    let entries = commands(
        owner
            .prepare(Intent::Queue {
                action: QueueAction::Move {
                    run_id: "second".into(),
                    before_run_id: Some("first".into()),
                },
            })
            .unwrap(),
    );
    assert_eq!(entries.len(), 1);
    let Request::Dispatch(dispatch) = &entries[0].request else {
        panic!("dispatch")
    };
    assert_eq!(
        dispatch.command,
        Command::ReorderQueued {
            run: agent_domain::RunId::new("second").unwrap(),
            before: Some(agent_domain::RunId::new("first").unwrap()),
        }
    );
}

fn question_request(multiple: bool) -> agent_domain::Request {
    agent_domain::Request {
        owner_path: vec![],
        id: agent_domain::RuntimeRequestId::new("request").unwrap(),
        attempt: agent_domain::RunAttemptId::new("attempt").unwrap(),
        native_key: "native".into(),
        body: agent_domain::RequestBody::Questions {
            questions: vec![agent_domain::Question {
                required: true,
                id: "areas".into(),
                header: "Areas".into(),
                question: "Which areas?".into(),
                multiple,
                options: vec![agent_domain::QuestionOption {
                    label: "Server".into(),
                    description: None,
                }],
            }],
        },
        capability: agent_domain::ResponseCapability::Message,
        status: agent_domain::RequestStatus::Pending,
        decision: None,
        answers: None,
        attachments: BTreeMap::new(),
        created_at: at(),
        resolved_at: None,
    }
}

#[test]
fn an_attachment_only_answer_is_empty_text_and_chosen_options_are_choices() {
    let mut state = thread_state("Thread");
    state.requests.push(question_request(true));
    let mut owner = opened(state);
    let key = answer_draft_key("request", "areas");
    owner.state.drafts.insert(
        key,
        Draft {
            attachments: vec![DraftAttachment {
                id: "local".into(),
                remote_id: Some("upload".into()),
                name: "notes.txt".into(),
                mime_type: "text/plain".into(),
                kind: "file".into(),
                size_bytes: 4,
                local_path: String::new(),
                status: "ready".into(),
                error: None,
            }],
            ..Draft::default()
        },
    );
    let entries = commands(
        owner
            .prepare(Intent::SubmitAnswers {
                request_id: "request".into(),
            })
            .unwrap(),
    );
    let Request::Dispatch(dispatch) = &entries[0].request else {
        panic!("dispatch")
    };
    let Command::Respond {
        answers,
        attachments,
        ..
    } = &dispatch.command
    else {
        panic!("respond")
    };
    assert_eq!(
        answers.as_ref().unwrap()["areas"],
        agent_domain::Answer::Text(String::new())
    );
    assert_eq!(attachments["areas"][0].id, "upload");
    assert!(
        owner
            .state
            .drafts
            .keys()
            .all(|key| !key.starts_with("answer:"))
    );

    let mut state = thread_state("Thread");
    state.requests.push(question_request(true));
    let mut owner = opened(state);
    owner
        .prepare(Intent::EditAnswer {
            request_id: "request".into(),
            question_id: "areas".into(),
            edit: AnswerEdit::ToggleOption {
                value: "Server".into(),
            },
        })
        .unwrap();
    let entries = commands(
        owner
            .prepare(Intent::SubmitAnswers {
                request_id: "request".into(),
            })
            .unwrap(),
    );
    let Request::Dispatch(dispatch) = &entries[0].request else {
        panic!("dispatch")
    };
    let Command::Respond { answers, .. } = &dispatch.command else {
        panic!("respond")
    };
    assert_eq!(
        answers.as_ref().unwrap()["areas"],
        agent_domain::Answer::Choices(vec!["Server".into()])
    );
}

#[test]
fn a_typed_answer_moves_to_the_thread_draft_when_an_option_replaces_it() {
    let mut state = thread_state("Thread");
    state.requests.push(question_request(false));
    let mut owner = opened(state);
    for edit in [
        AnswerEdit::Custom {
            text: "Only the server".into(),
        },
        AnswerEdit::ToggleOption {
            value: "Server".into(),
        },
    ] {
        owner
            .prepare(Intent::EditAnswer {
                request_id: "request".into(),
                question_id: "areas".into(),
                edit,
            })
            .unwrap();
    }
    assert_eq!(owner.state.current_draft().text, "Only the server");
    let drafts = &owner.state.question_drafts["request"];
    assert_eq!(
        drafts.get("areas").unwrap().selected_option_values,
        ["Server"]
    );
}

#[test]
fn a_stashed_draft_restores_its_text_and_uploaded_files() {
    let mut owner = opened(thread_state("Thread"));
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "Park this".into(),
            attachments: vec![DraftAttachment {
                id: "local".into(),
                remote_id: Some("upload".into()),
                name: "notes.txt".into(),
                mime_type: "text/plain".into(),
                kind: "file".into(),
                size_bytes: 4,
                local_path: String::new(),
                status: "ready".into(),
                error: None,
            }],
            ..draft()
        },
    );
    let Next::Outcome(Outcome::Stashed { entry_id }) = owner.prepare(Intent::StashDraft).unwrap()
    else {
        panic!("stashed")
    };
    assert!(owner.state.current_draft().is_empty());
    assert_eq!(owner.state.stash.entries.len(), 1);
    let Next::Outcome(Outcome::StashRestored { images, warning }) =
        owner.prepare(Intent::RestoreStash { entry_id }).unwrap()
    else {
        panic!("restored")
    };
    assert!(images.is_empty() && warning.is_none());
    let restored = owner.state.current_draft();
    assert_eq!(restored.text, "Park this");
    assert_eq!(restored.attachments[0].remote_id.as_deref(), Some("upload"));
    assert!(owner.state.stash.entries.is_empty());
}

#[test]
fn a_drop_from_another_section_changes_its_lifecycle_and_takes_its_slot() {
    use crate::view::thread_order::{DropSection, MoveDestination, OrderSection, Placement};
    let id = |value: &str| ThreadId::new(value).unwrap();
    let mut pinned = row(&id("pinned"));
    pinned.pinned_at = Some(at());
    let mut settled = row(&id("settled"));
    settled.settled = Some(true);
    let mut owner = owner(Snapshot::default());
    live_shell(&mut owner, 1, vec![pinned, settled, row(&id("active"))]);
    let sent = |owner: &mut Owner, moved: &str, section: DropSection| {
        let order = if section == DropSection::Pinned {
            OrderSection::Pinned
        } else {
            OrderSection::Active
        };
        commands(
            owner
                .prepare(Intent::MoveThread {
                    thread_id: moved.into(),
                    section: order,
                    destination: MoveDestination::Drop {
                        target: None,
                        section: Some(section),
                        placement: Placement::Before,
                    },
                })
                .unwrap(),
        )
        .into_iter()
        .map(|entry| {
            let Request::Dispatch(dispatch) = entry.request else {
                panic!("dispatch")
            };
            (entry.thread.to_string(), dispatch.command)
        })
        .collect::<Vec<_>>()
    };
    let unsettled = sent(&mut owner, "settled", DropSection::Active);
    assert!(matches!(
        &unsettled[0],
        (thread, Command::Settle { settled: false, .. }) if thread == "settled"
    ));
    assert!(
        unsettled[1..]
            .iter()
            .all(|(_, command)| matches!(command, Command::ReorderActive { .. }))
    );
    let pinned = sent(&mut owner, "active", DropSection::Pinned);
    assert!(matches!(
        &pinned[0],
        (thread, Command::Pin { pinned: true, order: Some(_) }) if thread == "active"
    ));
    assert!(
        pinned
            .iter()
            .all(|(thread, command)| thread != "active" || matches!(command, Command::Pin { .. }))
    );
    let settle = sent(&mut owner, "pinned", DropSection::Settled);
    assert!(matches!(
        settle.as_slice(),
        [(thread, Command::Settle { settled: true, .. })] if thread == "pinned"
    ));
}

#[test]
fn attached_terminal_output_joins_the_draft_as_a_linked_record() {
    let mut owner = opened(thread_state("Thread"));
    owner.state.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "Why does this fail?".into(),
            ..draft()
        },
    );
    let output = crate::view::terminals::TerminalOutputContext {
        terminal_id: "term-1".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 2,
        line_end: 4,
        text: "error[E0308]: mismatched types".into(),
    };
    owner
        .prepare(Intent::AttachTerminalOutput { output })
        .unwrap();
    let draft = owner.state.current_draft();
    let records = &draft.context.as_ref().unwrap().records;
    assert_eq!(records.len(), 1);
    let id = records[0].0["contextId"].as_str().unwrap();
    assert_eq!(
        draft.text,
        format!(
            "Why does this fail? [Terminal 1 · visible lines 2–4](context://v1/terminal/{id}) "
        )
    );
}

#[test]
fn a_closed_setup_stream_keeps_its_last_snapshot_for_the_card() {
    let mut owner = opened(thread_state("Thread"));
    let setup = agent_domain::WorktreeSetupSnapshot {
        thread: thread_id(),
        phase: agent_domain::WorktreeSetupPhase::Running,
        started_at: at(),
        ended_at: None,
        branch: None,
        base_ref: None,
        worktree_path: None,
        setup_script: None,
        stages: vec![],
        error: None,
        sequence: 3,
    };
    owner.setup_update(&thread_id(), Some(setup.clone()));
    owner.setup_update(&thread_id(), None);
    assert!(owner.state.setups.is_empty());
    assert_eq!(owner.state.held_setups[&thread_id()], setup);
    owner.setup_update(&thread_id(), Some(setup));
    assert!(owner.state.held_setups.is_empty());
}

#[test]
fn new_thread_defaults_change_without_touching_the_open_thread() {
    let mut owner = opened(thread_state("Thread"));
    let before = owner.state.current_draft();
    let next = owner
        .prepare(Intent::SetDefaultModel {
            instance_id: "claude".into(),
            driver: agent_domain::Driver::Claude,
            model: "sonnet".into(),
            options: vec![],
        })
        .unwrap();
    assert!(matches!(next, Next::Done));
    let next = owner
        .prepare(Intent::SetDefaultRuntimeMode {
            mode: agent_domain::RuntimeMode::ApprovalRequired,
        })
        .unwrap();
    assert!(matches!(next, Next::Done));
    let default = &owner.state.default_draft;
    assert_eq!(
        (default.instance_id.as_str(), default.model.as_str()),
        ("claude", "sonnet")
    );
    assert_eq!(
        default.runtime_mode,
        agent_domain::RuntimeMode::ApprovalRequired
    );
    assert_eq!(owner.state.current_draft(), before);
    assert!(
        owner
            .prepare(Intent::SetDefaultModel {
                instance_id: "codex".into(),
                driver: agent_domain::Driver::Codex,
                model: String::new(),
                options: vec![],
            })
            .is_err()
    );
    assert_eq!(owner.state.default_draft.model, "sonnet");
}

#[test]
fn preferences_change_on_the_device_and_survive_a_restart() {
    let mut owner = owner(Snapshot::default());
    for intent in [
        Intent::SetTimestampFormat {
            format: crate::view::time::TimestampFormat::TwentyFourHour,
        },
        Intent::SetWorkingSection { enabled: true },
        Intent::ToggleFavoriteModel {
            instance_id: "codex".into(),
            model: "gpt".into(),
        },
        Intent::SetDiffIgnoreWhitespace { ignore: false },
        Intent::SetTerminalFontSize { size: 40.0 },
    ] {
        owner.prepare(intent).unwrap();
    }
    let restored =
        crate::persistence::decode(&crate::persistence::encode(&owner.state).unwrap()).unwrap();
    assert_eq!(restored.preferences, owner.state.preferences);
    assert!(restored.preferences.working_section);
    assert!(!restored.preferences.diff_ignore_whitespace);
    assert_eq!(restored.preferences.favorite_models.len(), 1);
    assert_eq!(restored.terminal_text_size().size, 14.0);
}

#[test]
fn a_saved_staged_model_takes_the_options_it_was_last_given() {
    use crate::view::models::{
        fixtures::{host_instance, host_model},
        staging::{StagedModel, remembered_model_options},
    };
    let effort = |value: &str| ModelOption {
        key: "reasoningEffort".into(),
        value: value.into(),
    };
    let mut owner = owner(Snapshot {
        providers: Some(vec![host_instance(
            "codex",
            agent_domain::Driver::Codex,
            vec![host_model("gpt-a", "A"), host_model("gpt-b", "B")],
        )]),
        ..Snapshot::default()
    });
    owner
        .prepare(Intent::SetModel {
            instance_id: "codex".into(),
            driver: agent_domain::Driver::Codex,
            model: "gpt-a".into(),
            options: vec![],
        })
        .unwrap();
    owner
        .prepare(Intent::SelectTrait {
            descriptor_id: "reasoningEffort".into(),
            choice: "medium".into(),
        })
        .unwrap();
    let memory = &owner.state.preferences.model_options;
    assert_eq!(
        remembered_model_options(memory, "codex", "gpt-a"),
        Some(&vec![effort("medium")])
    );
    let staged = StagedModel {
        instance_id: "codex".into(),
        driver: agent_domain::Driver::Codex,
        model: "gpt-b".into(),
        options: vec![],
    };
    owner
        .prepare(Intent::SaveStagedModel {
            staged: staged.clone(),
        })
        .unwrap();
    assert_eq!(owner.state.current_draft().model, "gpt-b");
    assert!(owner.state.current_draft().options.is_empty());
    owner
        .prepare(Intent::RememberModelOptions {
            instance_id: "codex".into(),
            model: "gpt-a".into(),
            options: vec![effort("high")],
        })
        .unwrap();
    owner
        .prepare(Intent::SaveStagedModel {
            staged: StagedModel {
                model: "gpt-a".into(),
                ..staged
            },
        })
        .unwrap();
    let draft = owner.state.current_draft();
    assert_eq!(draft.model, "gpt-a");
    assert_eq!(draft.options, [effort("high")]);
}

#[test]
fn attached_terminal_output_joins_the_threads_draft_as_a_context_link() {
    let mut owner = opened(thread_state("Thread"));
    let thread = thread_id().to_string();
    owner
        .prepare(Intent::EditDraft {
            text: "see".into(),
            base_text: None,
        })
        .unwrap();
    let attach = |start, end| Intent::AttachTerminalOutput {
        thread_id: thread.clone(),
        terminal_id: "term-2".into(),
        output: "$ make\nok\n\n".into(),
        start,
        end,
    };
    assert!(owner.prepare(attach(0, 5)).is_err());
    owner.prepare(attach(0, 1)).unwrap();
    let draft = owner.state.current_draft();
    let record = &draft.context.as_ref().unwrap().records[0].0;
    assert_eq!(record["kind"], "terminal");
    assert_eq!(record["text"], "$ make\nok");
    assert_eq!(record["label"], "Terminal 2 · visible lines 1–2");
    let id = record["contextId"].as_str().unwrap();
    assert_eq!(
        draft.text,
        format!("see [Terminal 2 · visible lines 1–2](context://v1/terminal/{id}) ")
    );
}

#[test]
fn dismissing_a_wake_records_the_visit_at_the_wake() {
    let mut owner = opened(thread_state("Thread"));
    let entries = commands(
        owner
            .prepare(Intent::Thread {
                thread_id: thread_id().to_string(),
                action: ThreadAction::Visit { at: at().millis() },
            })
            .unwrap(),
    );
    let Request::Dispatch(dispatch) = &entries[0].request else {
        panic!("dispatch")
    };
    assert_eq!(dispatch.command, Command::Visit { at: at() });
}

#[test]
fn discarding_a_draft_removes_only_that_draft() {
    let mut owner = owner(Snapshot::default());
    for key in ["new:app", "thread"] {
        owner.state.drafts.insert(
            key.into(),
            Draft {
                text: "unsent".into(),
                ..Draft::default()
            },
        );
    }
    assert!(matches!(
        owner
            .prepare(Intent::DiscardDraft {
                draft_key: "new:app".into()
            })
            .unwrap(),
        Next::Done
    ));
    assert_eq!(
        owner.state.drafts.keys().cloned().collect::<Vec<_>>(),
        ["thread"]
    );
}

fn context_records(owner: &Owner) -> Vec<serde_json::Value> {
    owner
        .state
        .current_draft()
        .context
        .map(|context| context.records.into_iter().map(|record| record.0).collect())
        .unwrap_or_default()
}

#[test]
fn terminal_lines_join_the_draft_at_the_caret_once_per_range() {
    let mut owner = owner(Snapshot {
        default_draft: draft(),
        ..Snapshot::default()
    });
    let selection = crate::view::composer::terminal_context::TerminalContextSelection {
        terminal_id: "default".into(),
        terminal_label: "Terminal 1".into(),
        line_start: 4,
        line_end: 4,
        text: "\nls\n".into(),
    };
    let add = |owner: &mut Owner, text: &str, cursor| {
        owner
            .prepare(Intent::AddTerminalContext {
                text: text.into(),
                cursor,
                selection: selection.clone(),
            })
            .unwrap()
    };
    let Next::Outcome(Outcome::ComposerEdited { cursor }) = add(&mut owner, "seeit", 3) else {
        panic!("edited")
    };
    let records = context_records(&owner);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["text"], "ls");
    let link = format!(
        "[Terminal 1 line 4](context://v1/terminal/{})",
        records[0]["contextId"].as_str().unwrap()
    );
    assert_eq!(owner.state.current_draft().text, format!("see {link} it"));
    assert_eq!(cursor as usize, 4 + link.len() + 1);
    assert!(matches!(add(&mut owner, "", 0), Next::Done));
    assert_eq!(context_records(&owner).len(), 1);
}

#[test]
fn dropped_threads_attach_once_each_at_the_caret() {
    let mut owner = owner(Snapshot {
        default_draft: draft(),
        ..Snapshot::default()
    });
    let first = ThreadId::new("first").unwrap();
    let second = ThreadId::new("second").unwrap();
    live_shell(&mut owner, 1, vec![row(&first), row(&second)]);
    let drop = |owner: &mut Owner, ids: &[&str]| {
        let text = owner.state.current_draft().text;
        let cursor = text.encode_utf16().count() as u32;
        owner
            .prepare(Intent::AddThreadContexts {
                text,
                cursor,
                thread_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            })
            .unwrap()
    };
    assert!(matches!(
        drop(&mut owner, &["first", "gone"]),
        Next::Outcome(Outcome::ComposerEdited { .. })
    ));
    assert!(matches!(
        drop(&mut owner, &["first", "second"]),
        Next::Outcome(Outcome::ComposerEdited { .. })
    ));
    assert!(matches!(drop(&mut owner, &["second"]), Next::Done));
    let ids: Vec<_> = context_records(&owner)
        .iter()
        .map(|record| record["threadId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["first", "second"]);
    assert_eq!(
        owner.state.current_draft().text,
        "[Thread](context://v1/thread/thread_first) [Thread](context://v1/thread/thread_second) "
    );
}

fn project_shell(owner: &mut Owner) {
    owner.shell_update(
        ShellLocation::Active,
        ShellUpdate::Snapshot(ShellSnapshot {
            snapshot_sequence: 1,
            projects: vec![crate::models::Project {
                id: "app".into(),
                name: "App".into(),
                roots: vec![crate::models::ProjectRoot {
                    path: "/repo".into(),
                }],
                ..Default::default()
            }],
            threads: vec![],
        }),
    );
}

// mobile queries.ts useComposerPathSearch: the debounced query is asked once
// and only its own answer is shown.
#[test]
fn a_path_trigger_waits_for_its_debounce_and_keeps_only_its_own_answer() {
    use agent_protocol::workspace as w;
    let mut owner = owner(Snapshot {
        default_draft: draft(),
        selected_project: Some("app".into()),
        ..Snapshot::default()
    });
    project_shell(&mut owner);
    owner.update_composer_menu(
        "see @src",
        8,
        crate::view::timeline::rows::TimelineLayout::Mobile,
    );
    let entries = &owner.state.sources.entries;
    let wanted = entries.wanted.clone().unwrap();
    assert_eq!(
        (wanted.cwd.as_str(), wanted.query.as_str(), wanted.limit),
        ("/repo", "src", 20)
    );
    assert!(entries.due_at_ms.is_some());
    let answer = |query: &str| {
        (
            w::SearchEntries {
                cwd: "/repo".into(),
                query: query.into(),
                limit: 20,
                kind: None,
                image_only: false,
            },
            w::EntrySearch {
                entries: vec![w::WorkspaceEntry {
                    path: format!("{query}/lib.rs"),
                    kind: w::EntryKind::File,
                }],
                truncated: false,
            },
        )
    };
    let (stale, found) = answer("sr");
    owner.entries_found(&stale, found);
    assert_eq!(owner.state.sources.entries.result, None);
    let (current, found) = answer("src");
    owner.entries_found(&current, found);
    let menu = owner.state.composer_menu("see @src".into(), 8);
    assert_eq!(menu.items[0].label, "lib.rs");
    owner.update_composer_menu(
        "plain",
        5,
        crate::view::timeline::rows::TimelineLayout::Mobile,
    );
    assert_eq!(owner.state.sources.entries.wanted, None);
}

// mobile ThreadRouteScreen handleWorkLocally.
#[test]
fn working_locally_relaunches_the_first_message_once_the_setup_is_cancelled() {
    let mut state = thread_state("Thread");
    state.messages.push(message("first", "Fix the bug"));
    let mut owner = opened(state);
    let next = owner.work_locally().unwrap();
    assert!(
        matches!(next, Next::Call(call, _) if matches!(*call, crate::protocol::Call::CancelSetup(_)))
    );
    owner.setup_cancelled(&thread_id(), false);
    assert!(owner.state.outbox.entries.is_empty());
    owner.work_locally().unwrap();
    owner.setup_cancelled(&thread_id(), true);
    let entry = owner.state.outbox.entries[0].clone();
    let Request::Launch(launch) = &entry.request else {
        panic!("launch")
    };
    assert_eq!(launch.project_id, "project");
    assert_eq!(
        launch.workspace,
        agent_protocol::conversation::WorkspaceStrategy::Root { branch: None }
    );
    assert_eq!(launch.message.as_ref().unwrap().text, "Fix the bug");
    assert!(entry.navigate);
}

// NewTaskDraftScreen initialProjectRef.branch: a local draft that reuses the
// branch's worktree.
#[test]
fn a_new_thread_on_a_branch_opens_a_local_draft_in_its_worktree() {
    let mut owner = owner(Snapshot {
        default_draft: draft(),
        ..Snapshot::default()
    });
    project_shell(&mut owner);
    owner.state.workspace.worktree_settings = Some(crate::models::WorktreeSettings {
        create_on_new_session: true,
        ..Default::default()
    });
    let (sender, _receipt) = oneshot::channel();
    owner.intent(
        Intent::NewThreadOnBranch {
            project_id: "app".into(),
            branch: "fix".into(),
            worktree_path: Some("/trees/fix".into()),
        },
        sender,
    );
    assert_eq!(owner.state.selected_project.as_deref(), Some("app"));
    let workspace = owner.state.new_thread_workspace();
    assert_eq!(
        workspace,
        DraftWorkspace {
            mode: crate::view::projects::selection::ThreadWorkspaceMode::Local,
            branch: Some("fix".into()),
            worktree_path: Some("/trees/fix".into()),
            start_from_origin: false,
        }
    );
    assert_eq!(owner.state.composer_cwd(), "/trees/fix");
}

// mobile queries.ts usePaginatedBranches: later pages join the first.
#[test]
fn a_later_branch_page_joins_the_first() {
    use agent_protocol::workspace as w;
    let mut owner = owner(Snapshot {
        selected_project: Some("app".into()),
        ..Snapshot::default()
    });
    project_shell(&mut owner);
    let branch = |name: &str| w::VcsRef {
        name: name.into(),
        is_remote: false,
        remote_name: None,
        current: false,
        is_default: false,
        worktree_path: None,
    };
    let request = |cursor| w::ListRefs {
        cwd: "/repo".into(),
        query: None,
        cursor,
        include_matching_remote_refs: false,
        ref_kind: w::RefKind::All,
        limit: Some(100),
    };
    owner.state.sources.refs.insert(
        ("/repo".into(), RefScope::All),
        RefsEntry {
            in_flight: true,
            ..Default::default()
        },
    );
    owner.refs_finished(
        &request(None),
        Ok(w::RefList {
            refs: vec![branch("main"), branch("dev")],
            is_repo: true,
            has_primary_remote: false,
            next_cursor: Some(2),
            total_count: 3,
        }),
    );
    let view = owner
        .state
        .new_thread(Default::default())
        .workspace
        .unwrap();
    assert!(view.has_more_branches);
    owner
        .state
        .sources
        .refs
        .get_mut(&("/repo".into(), RefScope::All))
        .unwrap()
        .in_flight = true;
    owner.refs_finished(
        &request(Some(2)),
        Ok(w::RefList {
            refs: vec![branch("dev"), branch("topic")],
            is_repo: true,
            has_primary_remote: false,
            next_cursor: None,
            total_count: 3,
        }),
    );
    let view = owner
        .state
        .new_thread(Default::default())
        .workspace
        .unwrap();
    let names: Vec<&str> = view.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main", "dev", "topic"]);
    assert!(!view.has_more_branches);
}

// checkout-new-task-branch.ts: a local branch checked out nowhere is switched
// to before the draft takes it.
#[test]
fn picking_another_local_branch_switches_the_checkout_first() {
    use agent_protocol::workspace as w;
    let mut owner = owner(Snapshot {
        selected_project: Some("app".into()),
        ..Snapshot::default()
    });
    project_shell(&mut owner);
    let branch = |name: &str, current: bool| w::VcsRef {
        name: name.into(),
        is_remote: false,
        remote_name: None,
        current,
        is_default: false,
        worktree_path: current.then(|| "/repo".into()),
    };
    owner.state.sources.refs.insert(
        ("/repo".into(), RefScope::All),
        RefsEntry {
            list: Some(w::RefList {
                refs: vec![branch("main", true), branch("topic", false)],
                is_repo: true,
                has_primary_remote: false,
                next_cursor: None,
                total_count: 2,
            }),
            ..Default::default()
        },
    );
    let next = owner
        .select_new_thread_branch("main".into(), Some("/repo".into()))
        .unwrap();
    assert!(matches!(next, Next::Done));
    let next = owner
        .select_new_thread_branch("topic".into(), None)
        .unwrap();
    let Next::Call(call, _) = next else {
        panic!("switch")
    };
    let crate::protocol::Call::SwitchRef(request) = *call else {
        panic!("switch")
    };
    assert_eq!(request.ref_name, "topic");
    owner.switched_ref(
        &request,
        w::SwitchedRef {
            ref_name: Some("topic".into()),
        },
    );
    assert_eq!(
        owner.state.new_thread_workspace().branch.as_deref(),
        Some("topic")
    );
}

// web chatThreadActions.test.ts "only applies the start-from-origin default
// to new worktree drafts" and ChatView onEnvModeChange: a worktree draft
// starts from origin when the project's setting says so, a local one never.
#[test]
fn only_applies_the_start_from_origin_default_to_new_worktree_drafts() {
    use crate::view::projects::selection::ThreadWorkspaceMode;
    let mut owner = owner(Snapshot {
        default_draft: draft(),
        selected_project: Some("app".into()),
        ..Snapshot::default()
    });
    project_shell(&mut owner);
    owner.state.workspace.worktree_settings = Some(crate::models::WorktreeSettings {
        create_on_new_session: true,
        ..Default::default()
    });
    let workspace = owner.state.new_thread_workspace();
    assert_eq!(
        (workspace.mode, workspace.start_from_origin),
        (ThreadWorkspaceMode::Worktree, true)
    );
    owner
        .set_new_thread_workspace(ThreadWorkspaceMode::Local)
        .unwrap();
    assert!(!owner.state.new_thread_workspace().start_from_origin);

    let mut settings = crate::models::ConversationSettings::default();
    settings.project_overrides.insert(
        "app".into(),
        crate::models::ProjectConversationSettings {
            new_worktrees_start_from_origin: Some(false),
            ..Default::default()
        },
    );
    owner.state.conversation_settings = Some(settings);
    owner
        .set_new_thread_workspace(ThreadWorkspaceMode::Worktree)
        .unwrap();
    assert!(!owner.state.new_thread_workspace().start_from_origin);
    owner.set_new_thread_start_from_origin(true).unwrap();
    assert!(owner.state.new_thread_workspace().start_from_origin);
}

// BranchToolbarBranchSelector createRef: the searched name, sanitized, is
// created on the checkout and switched to, and the draft works on it.
#[test]
fn creating_a_new_ref_switches_the_checkout_and_the_draft_takes_it() {
    use agent_protocol::workspace as w;
    let mut owner = owner(Snapshot {
        selected_project: Some("app".into()),
        ..Snapshot::default()
    });
    project_shell(&mut owner);
    let Next::Call(call, _) = owner
        .create_new_thread_branch(" new thing ".into())
        .unwrap()
    else {
        panic!("create")
    };
    let crate::protocol::Call::CreateRef(request) = *call else {
        panic!("create")
    };
    assert_eq!(
        request,
        w::CreateRef {
            cwd: "/repo".into(),
            ref_name: "new-thing".into(),
            switch_ref: true,
        }
    );
    owner.switched_ref(
        &w::SwitchRef {
            cwd: request.cwd,
            ref_name: request.ref_name,
        },
        w::SwitchedRef {
            ref_name: Some("new-thing".into()),
        },
    );
    assert_eq!(
        owner.state.new_thread_workspace().branch.as_deref(),
        Some("new-thing")
    );
    assert!(matches!(
        owner.create_new_thread_branch("   ".into()).unwrap(),
        Next::Done
    ));
}

#[test]
fn a_due_provider_command_retry_waits_for_a_connection() {
    let mut owner = owner(Snapshot::default());
    owner.state.sources.provider_commands.insert(
        ("codex".into(), "/repo".into()),
        ProviderCommandsEntry {
            retry_at_ms: Some(0),
            ..ProviderCommandsEntry::default()
        },
    );
    assert_eq!(owner.sources_deadline(), None);
}

#[test]
fn a_replaced_connection_releases_the_requests_it_carried() {
    let mut state = thread_state("Thread");
    let mut item = command_item("command", 1);
    item.output_omitted = true;
    state.items.push(item.clone());
    let mut owner = owner(Snapshot::default());
    owner.select_thread(Some(thread_id()));
    owner.thread_update(
        &thread_id(),
        snapshot(state, 1, Some(window(Some("cursor"), true, Some(1)))),
    );
    let sync = Arc::make_mut(owner.state.threads.get_mut(&thread_id()).unwrap());
    assert!(sync.begin_detail(&item.id));
    assert_eq!(
        sync.begin_load_earlier(),
        crate::sync::thread::LoadEarlier::Request("cursor".into())
    );
    let key = ("codex".to_owned(), "/repo".to_owned());
    owner.state.sources.provider_commands.insert(
        key.clone(),
        ProviderCommandsEntry {
            in_flight: true,
            ..ProviderCommandsEntry::default()
        },
    );
    owner.abandon_requests();
    let sync = &owner.state.threads[&thread_id()];
    assert!(!sync.history.loading);
    assert_eq!(sync.details.get(&item.id), None);
    let entry = &owner.state.sources.provider_commands[&key];
    assert!(!entry.in_flight);
    assert!(entry.retry_at_ms.is_some());
}

#[tokio::test]
async fn a_command_reaches_the_host_only_once_the_device_state_holds_it() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("device.json");
        let mut host = Loopback::connect(
            std::time::Duration::from_secs(5),
            StoreOptions {
                state_file: Some(state_file.clone()),
                ..options()
            },
            |_| {},
        )
        .await;
        live_shell(&mut host.owner, 1, vec![row(&thread_id())]);
        let entry = host.owner.pending(
            thread_id(),
            Command::Pin {
                pinned: true,
                order: None,
            },
        );
        host.owner.enqueue(entry.clone(), None).unwrap();
        host.owner.publish();
        assert_eq!(host.owner.state.outbox.entries[0].phase, Phase::Queued);
        host.owner.tick();
        let written = host.events.recv().await.unwrap();
        assert!(matches!(written, Event::Written(owner::Written::Device, true)));
        let saved = crate::persistence::load(&state_file, &[]);
        assert_eq!(saved.outbox.entries[0].id, entry.id);
        host.owner.handle(written).await;
        assert_eq!(host.owner.state.outbox.entries[0].phase, Phase::InFlight);
        let request = host.next_call().await;
        assert!(matches!(&request.call, Call::Dispatch(dispatch) if dispatch.command_id == entry.id));
        host.close().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn closing_writes_the_latest_device_state() {
    let directory = tempfile::tempdir().unwrap();
    let state_file = directory.path().join("device.json");
    let store = Store::offline(
        Snapshot::default(),
        StoreOptions {
            state_file: Some(state_file.clone()),
            ..options()
        },
    );
    store
        .dispatch(Intent::EditDraft {
            base_text: None,
            text: "typed just before closing".into(),
        })
        .await
        .unwrap()
        .unwrap();
    store.close().await.unwrap();
    assert_eq!(
        crate::persistence::load(&state_file, &[]).current_draft().text,
        "typed just before closing"
    );
}

#[tokio::test]
async fn a_deleted_thread_cache_stays_deleted_after_an_earlier_save_lands() {
    let directory = tempfile::tempdir().unwrap();
    let (sender, _events) = mpsc::channel(8);
    let mut owner = Owner::new(
        Snapshot::default(),
        StoreOptions {
            cache_directory: Some(directory.path().into()),
            ..options()
        },
        sender,
    )
    .0;
    owner.select_thread(Some(thread_id()));
    owner.thread_update(&thread_id(), snapshot(thread_state("Thread"), 1, None));
    owner.thread_update(&thread_id(), ThreadUpdate::Synchronized);
    owner.store_thread_now(&thread_id());
    owner.thread_deleted(&thread_id());
    owner.teardown().await;
    assert!(
        crate::sync::DiskCache::new(directory.path())
            .load_thread(&thread_id())
            .is_none()
    );
}

// web useReviewFilePatches with client-runtime diffFilePatch: a truncated
// source that lists its files is read file by file, four reads at a time; a
// newer preview reads the asked files again and a new diff starts over.
#[test]
fn a_truncated_diff_with_its_file_list_is_read_file_by_file() {
    use agent_protocol::workspace as w;
    let mut owner = opened(thread_state("Thread"));
    let request = w::DiffPreview {
        cwd: owner.state.cwd(),
        base_ref: None,
        ignore_whitespace: false,
        file: None,
    };
    let source = |hash: &str, truncated: bool, paths: &[&str]| {
        w::DiffSource {
        id: "branch-range".into(),
        kind: w::DiffSourceKind::BranchRange,
        title: "Changes vs main".into(),
        base_ref: Some("main".into()),
        head_ref: Some("topic".into()),
        diff: paths
            .iter()
            .map(|path| {
                format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+b\n")
            })
            .collect(),
        diff_hash: hash.into(),
        truncated,
        files: Some(
            paths
                .iter()
                .map(|path| w::DiffFile {
                    path: (*path).into(),
                    previous_path: None,
                    additions: 1,
                    deletions: 1,
                })
                .collect(),
        ),
    }
    };
    let preview = |owner: &mut Owner, millis: i64, source: w::DiffSource| {
        owner.state.sources.diff_preview = Some(DiffPreviewEntry {
            request: request.clone(),
            result: None,
            error: None,
        });
        owner.finished(job(
            Call::DiffPreview(request.clone()),
            Ok(CallReply::DiffPreview(w::DiffPreviewResult {
                cwd: request.cwd.clone(),
                generated_at: agent_domain::Timestamp::from_millis(millis).unwrap(),
                sources: vec![source],
            })),
            None,
        ));
    };
    let files = ["f.ts", "e.ts", "d.ts", "c.ts", "b.ts", "a.ts"];
    let file_request = |owner: &Owner, path: &str| {
        let entry = owner.state.sources.diff_files.as_ref().unwrap();
        let file = entry.files.iter().find(|file| file.path == path).unwrap();
        Call::DiffPreview(entry.file_request(file))
    };
    let reading = |owner: &Owner| -> Vec<String> {
        let entry = owner.state.sources.diff_files.as_ref().unwrap();
        entry
            .patches
            .iter()
            .filter(|(_, patch)| patch.in_flight)
            .map(|(path, _)| path.clone())
            .collect()
    };
    preview(&mut owner, 1, source("one", true, &files));
    assert_eq!(reading(&owner), ["a.ts", "b.ts", "c.ts", "d.ts"]);
    let call = file_request(&owner, "a.ts");
    let Call::DiffPreview(single) = &call else {
        unreachable!()
    };
    assert_eq!(
        single.file,
        Some(w::DiffPreviewFile {
            path: "a.ts".into(),
            previous_path: None,
            source: w::DiffSourceKind::BranchRange,
        })
    );
    assert_eq!(single.base_ref.as_deref(), Some("main"));
    owner.finished(job(
        call,
        Ok(CallReply::DiffPreview(w::DiffPreviewResult {
            cwd: request.cwd.clone(),
            generated_at: agent_domain::Timestamp::from_millis(2).unwrap(),
            sources: vec![source("a", false, &["a.ts"])],
        })),
        None,
    ));
    owner.prepare(Intent::LoadMoreDiffFiles).unwrap();
    assert_eq!(reading(&owner), ["b.ts", "c.ts", "d.ts", "e.ts"]);
    owner
        .prepare(Intent::RevealDiffFile {
            path: "f.ts".into(),
            retry: false,
        })
        .unwrap();
    let entry = owner.state.sources.diff_files.as_ref().unwrap();
    assert_eq!(entry.queue, ["f.ts"]);
    let selection = crate::view::checkpoints::DiffPanelSelection::default();
    let cwd = owner.state.cwd();
    let view = crate::view::review_files::review_files_view(
        crate::view::review_files::lazy_entry(&owner.state, &cwd, &selection).unwrap(),
    );
    assert_eq!((view.settled_count, view.additions), (1, 6));
    assert!(view.pending);
    let git = crate::view::checkpoints::git_diff_view(&owner.state, &cwd, &selection);
    assert!(!git.truncated && git.files_revision.is_some());

    let failed = file_request(&owner, "b.ts");
    owner.finished(job(failed, Err(invalid("offline")), None));
    assert_eq!(reading(&owner), ["c.ts", "d.ts", "e.ts", "f.ts"]);
    let patches = &owner.state.sources.diff_files.as_ref().unwrap().patches;
    assert!(matches!(patches["b.ts"].result, Some(Err(_))));
    owner
        .prepare(Intent::RevealDiffFile {
            path: "b.ts".into(),
            retry: true,
        })
        .unwrap();
    assert_eq!(
        owner.state.sources.diff_files.as_ref().unwrap().queue,
        ["b.ts"]
    );

    // The same diff again: every asked file is read again.
    for path in ["c.ts", "d.ts", "e.ts", "f.ts"] {
        let call = file_request(&owner, path);
        owner.finished(job(call, Err(invalid("offline")), None));
    }
    preview(&mut owner, 3, source("one", true, &files));
    let entry = owner.state.sources.diff_files.as_ref().unwrap();
    assert_eq!(entry.patches.len(), 6);
    assert_eq!(reading(&owner).len() + entry.queue.len(), 6);
    assert_eq!(
        entry.patches["a.ts"].result.as_ref().map(Result::is_ok),
        Some(true)
    );

    // A new diff starts over, and a late read of the old one is dropped.
    let late = file_request(&owner, "a.ts");
    preview(&mut owner, 4, source("two", true, &files));
    owner.finished(job(late, Err(invalid("offline")), None));
    let entry = owner.state.sources.diff_files.as_ref().unwrap();
    assert_eq!(entry.patches.len(), 4);
    assert!(entry.patches.values().all(|patch| patch.result.is_none()));
    assert_eq!(reading(&owner), ["a.ts"]);

    preview(&mut owner, 5, source("three", false, &files));
    assert!(owner.state.sources.diff_files.is_none());
}
