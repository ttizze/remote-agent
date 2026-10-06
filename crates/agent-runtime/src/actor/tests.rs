use super::*;
use crate::store::tests::{at, selection, temp_store, thread};
use crate::{CommitListener, CommitNotice, ManualClock, OutboxQueue, SqliteOutbox, StoredFact};
use agent_domain::{
    DispatchMode, EffectBody, InteractionMode, ItemKind, MessageAuthor, MessageId, ProviderCommand,
    ProviderItem, ProviderOperation, RunId, RunStatus, RuntimeMode, SendMessage, fold,
};
use std::sync::Mutex;

struct Harness {
    _dir: tempfile::TempDir,
    context: ActorContext,
    clock: Arc<ManualClock>,
    notices: Arc<Notices>,
}
#[derive(Default)]
struct Notices(Mutex<Vec<CommitNotice>>);
impl CommitListener for Notices {
    fn committed(&self, notice: &CommitNotice) {
        self.0.lock().unwrap().push(notice.clone());
    }
}
impl Notices {
    fn effects(&self) -> usize {
        self.0.lock().unwrap().iter().map(|n| n.effects.len()).sum()
    }
    fn global_seqs(&self) -> Vec<u64> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .flat_map(|n| n.facts.iter().map(|f| f.global_seq))
            .collect()
    }
}
fn harness() -> Harness {
    let (dir, store) = temp_store();
    let notices = Arc::new(Notices::default());
    store.add_listener(notices.clone());
    let clock = Arc::new(ManualClock::new(&at()));
    let mut context = ActorContext::new(store);
    context.clock = clock.clone();
    Harness {
        _dir: dir,
        context,
        clock,
        notices,
    }
}
pub(crate) fn command_id(id: &str) -> CommandId {
    CommandId::new(id).unwrap()
}
pub(crate) fn create(thread: &ThreadId) -> Command {
    Command::Create {
        workspace: None,
        thread: thread.clone(),
        project: "project".into(),
        title: "Thread".into(),
        selection: selection(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        created_by: agent_domain::MessageAuthor::User,
        creation_source: "desktop".into(),
    }
}
pub(crate) fn send(id: &str) -> Command {
    Command::Send(SendMessage {
        context: None,
        title_seed: None,
        created_by: MessageAuthor::User,
        creation_source: "client".into(),
        id: MessageId::new(id).unwrap(),
        text: id.into(),
        attachments: vec![],
        selection: None,
        mode: DispatchMode::StartImmediately,
        intent: None,
        source_plan: None,
        resolved_plan: None,
        continuation: None,
    })
}
fn rename(title: &str) -> Command {
    Command::Rename {
        title: title.into(),
    }
}
pub(crate) async fn created(context: &ActorContext, id: &ThreadId) -> ActorHandle {
    let handle = ActorHandle::spawn(context.clone(), id.clone())
        .await
        .unwrap();
    let committed = handle
        .dispatch(
            command_id(&format!("create:{id}")),
            create(id),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert_eq!(committed.reply, Reply::Thread(id.clone()));
    handle
}
fn stored_facts(context: &ActorContext, id: &ThreadId) -> Vec<StoredFact> {
    context.store.facts_after(Some(id), 0).unwrap()
}
fn folded(context: &ActorContext, id: &ThreadId) -> State {
    let facts: Vec<_> = stored_facts(context, id)
        .into_iter()
        .map(|f| f.fact)
        .collect();
    fold(&State::default(), &facts).unwrap()
}
async fn started_run(handle: &ActorHandle, message: &str) -> (RunId, RunAttemptId) {
    let committed = handle
        .dispatch(command_id(message), send(message), CommandOrigin::Client)
        .await
        .unwrap();
    let Reply::Run(run) = committed.reply else {
        panic!("{committed:?}")
    };
    let view = handle.view().await.unwrap();
    let attempt = view
        .state
        .runs
        .iter()
        .find(|r| r.id == run)
        .unwrap()
        .attempt
        .clone()
        .unwrap();
    for event in [
        ProviderEvent::SessionReady {
            native_thread: "native-thread".into(),
        },
        ProviderEvent::TurnStarted {
            native_turn: Some("native-turn".into()),
        },
        ProviderEvent::ItemStarted {
            key: "answer".into(),
            kind: ProviderItem::Text,
        },
    ] {
        handle.provider(attempt.clone(), event).await.unwrap();
    }
    (run, attempt)
}

#[tokio::test]
async fn commits_facts_receipt_and_outbox_in_one_step() {
    let h = harness();
    let id = thread("thread:commit");
    let handle = created(&h.context, &id).await;
    let committed = handle
        .dispatch(command_id("send"), send("hello"), CommandOrigin::Client)
        .await
        .unwrap();
    assert!(matches!(committed.reply, Reply::Run(_)));
    let facts = stored_facts(&h.context, &id);
    assert_eq!(committed.thread_seq, facts.len() as u64);
    assert_eq!(committed.global_seq, facts.last().unwrap().global_seq);
    let receipt = h
        .context
        .store
        .receipt(&command_id("send"))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.thread, id);
    assert_eq!(receipt.receipt.reply, committed.reply);
    assert_eq!(
        (receipt.thread_seq, receipt.global_seq),
        (committed.thread_seq, committed.global_seq)
    );
    let outbox = h.context.store.outbox(&id).unwrap();
    assert!(
        outbox.iter().any(|row| row.kind == "Provider.Start"
            && row.status == crate::EffectStatus::Pending
            && row.lane == "main"),
        "{outbox:?}"
    );
    assert!(
        outbox
            .iter()
            .all(|row| row.effect.id.contains("thread:commit#"))
    );
    assert_eq!(
        handle.view().await.unwrap().state.as_ref(),
        &folded(&h.context, &id)
    );
    let head = h.context.store.thread_head(&id).unwrap();
    assert_eq!(head.input_seq, 2);
    assert_eq!(
        h.context.store.threads_needing_recovery().unwrap(),
        std::slice::from_ref(&id)
    );
    let indexed: (String, String) = h
        .context
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT role, text FROM search_messages WHERE thread_id = 'thread:commit'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(indexed, ("user".to_string(), "hello".to_string()));
    let (_, shell) = h.context.store.shell(&id).unwrap().unwrap();
    assert!(shell.needs_recovery);
}

#[tokio::test]
async fn keeps_one_durable_effect_across_command_retries() {
    let h = harness();
    let id = thread("thread:foundation-effect-recovery");
    let handle = created(&h.context, &id).await;
    let first = handle
        .dispatch(
            command_id("command:foundation-effect-recovery"),
            send("m"),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    let effects = h.notices.effects();
    let retry = handle
        .dispatch(
            command_id("command:foundation-effect-recovery"),
            send("m"),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert!(!first.replayed);
    assert!(retry.replayed);
    assert_eq!(
        (retry.reply.clone(), retry.thread_seq, retry.global_seq),
        (first.reply.clone(), first.thread_seq, first.global_seq)
    );
    assert_eq!(
        h.context
            .store
            .outbox(&id)
            .unwrap()
            .iter()
            .filter(|row| row.kind == "Provider.Start")
            .count(),
        1
    );
    assert_eq!(
        h.notices.effects(),
        effects,
        "an idempotent retry wakes no claimer"
    );

    drop(handle);
    let reloaded = ActorHandle::spawn(h.context.clone(), id.clone())
        .await
        .unwrap();
    let after_restart = reloaded
        .dispatch(
            command_id("command:foundation-effect-recovery"),
            send("m"),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert_eq!(
        after_restart,
        Committed {
            replayed: true,
            ..first
        }
    );
}

#[tokio::test]
async fn rejects_a_reused_command_id() {
    let h = harness();
    let first = thread("thread:first");
    let second = thread("thread:second");
    let first_handle = created(&h.context, &first).await;
    let second_handle = created(&h.context, &second).await;
    first_handle
        .dispatch(command_id("shared"), rename("one"), CommandOrigin::Client)
        .await
        .unwrap();
    let before = h.context.store.thread_head(&second).unwrap();
    let conflict = second_handle
        .dispatch(command_id("shared"), rename("one"), CommandOrigin::Client)
        .await
        .unwrap();
    assert_eq!(
        conflict.reply,
        Reply::Rejected {
            reason: "command-id-conflict".into()
        }
    );
    assert_eq!(h.context.store.thread_head(&second).unwrap(), before);
    let changed = first_handle
        .dispatch(command_id("shared"), rename("two"), CommandOrigin::Client)
        .await
        .unwrap();
    assert_eq!(
        changed.reply,
        Reply::Rejected {
            reason: "command-id-conflict".into()
        }
    );
    assert_eq!(
        first_handle
            .view()
            .await
            .unwrap()
            .state
            .thread
            .as_ref()
            .unwrap()
            .title,
        "one"
    );
}

#[tokio::test]
async fn persists_rejections_as_receipts() {
    let h = harness();
    let id = thread("thread:rejected");
    let handle = created(&h.context, &id).await;
    let rejected = handle
        .dispatch(
            command_id("cancel"),
            Command::CancelQueued {
                run: RunId::new("missing").unwrap(),
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert!(
        matches!(rejected.reply, Reply::Rejected { .. }),
        "{rejected:?}"
    );
    let head = h.context.store.thread_head(&id).unwrap();
    assert_eq!(head.input_seq, 2);
    let retry = handle
        .dispatch(
            command_id("cancel"),
            Command::CancelQueued {
                run: RunId::new("missing").unwrap(),
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.reply, rejected.reply);
}

#[tokio::test]
async fn guards_internal_commands_and_thread_identity() {
    let h = harness();
    let id = thread("thread:guard");
    let handle = ActorHandle::spawn(h.context.clone(), id.clone())
        .await
        .unwrap();
    let other = handle
        .dispatch(
            command_id("create"),
            create(&thread("thread:other")),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert_eq!(
        other.reply,
        Reply::Rejected {
            reason: "thread-mismatch".into()
        }
    );
    let internal = handle
        .dispatch(
            command_id("release"),
            Command::ReleasePrepared {
                run: RunId::new("run").unwrap(),
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert_eq!(
        internal.reply,
        Reply::Rejected {
            reason: "internal-command".into()
        }
    );
    assert!(
        h.context
            .store
            .receipt(&command_id("release"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        h.context.store.thread_head(&id).unwrap(),
        ThreadHead::default()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_sequence_follows_commit_order_across_concurrent_actors() {
    let h = harness();
    let mut tasks = vec![];
    for index in 0..8 {
        let context = h.context.clone();
        tasks.push(tokio::spawn(async move {
            let id = thread(&format!("thread:{index}"));
            let handle = created(&context, &id).await;
            for step in 0..20 {
                handle
                    .dispatch(
                        command_id(&format!("{id}:{step}")),
                        rename(&format!("{step}")),
                        CommandOrigin::Client,
                    )
                    .await
                    .unwrap();
            }
            id
        }));
    }
    let mut threads = vec![];
    for task in tasks {
        threads.push(task.await.unwrap());
    }
    let published = h.notices.global_seqs();
    assert_eq!(published, (1..=published.len() as u64).collect::<Vec<_>>());
    for id in threads {
        let facts = stored_facts(&h.context, &id);
        assert_eq!(
            facts.iter().map(|f| f.thread_seq).collect::<Vec<_>>(),
            (1..=facts.len() as u64).collect::<Vec<_>>()
        );
        assert!(facts.windows(2).all(|w| w[0].global_seq < w[1].global_seq));
    }
}

#[tokio::test]
async fn loads_from_snapshot_and_tail_like_a_full_fold() {
    let h = harness();
    let id = thread("thread:snapshot");
    let handle = created(&h.context, &id).await;
    for step in 0..300 {
        handle
            .dispatch(
                command_id(&format!("rename:{step}")),
                rename(&format!("title {step}")),
                CommandOrigin::Client,
            )
            .await
            .unwrap();
    }
    let live = handle.view().await.unwrap();
    drop(handle);
    let snapshot_seq: i64 = h
        .context
        .store
        .read(|c| Ok(c.query_row("SELECT thread_seq FROM thread_snapshots", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(snapshot_seq, 256);
    let loaded = h.context.store.load_thread(&id).unwrap();
    assert_eq!(loaded.state, folded(&h.context, &id));
    assert_eq!(&loaded.state, live.state.as_ref());
    assert_eq!(loaded.head, live.head);

    h.context
        .store
        .write(|tx| Ok(tx.execute("UPDATE thread_snapshots SET format = 'retired'", [])?))
        .await
        .unwrap();
    let reloaded = ActorHandle::spawn(h.context.clone(), id.clone())
        .await
        .unwrap();
    assert_eq!(
        reloaded.view().await.unwrap().state.as_ref(),
        live.state.as_ref()
    );
    reloaded
        .dispatch(
            command_id("after-retire"),
            rename("after"),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    let (format, seq): (String, i64) = h
        .context
        .store
        .read(|c| {
            Ok(
                c.query_row("SELECT format, thread_seq FROM thread_snapshots", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .unwrap();
    assert_eq!((format.as_str(), seq), (crate::SNAPSHOT_FORMAT, 302));
}

#[tokio::test]
async fn a_failed_commit_discards_the_new_state() {
    let h = harness();
    let id = thread("thread:crash");
    let handle = created(&h.context, &id).await;
    let before = handle.view().await.unwrap();
    h.context
        .store
        .on_writer(|c| {
            c.execute_batch(
                "CREATE TEMP TRIGGER crash_outbox BEFORE INSERT ON outbox
                 BEGIN SELECT RAISE(ABORT, 'injected crash'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = handle
        .dispatch(command_id("send"), send("hello"), CommandOrigin::Client)
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::Store(_)), "{error}");
    let after = handle.view().await.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.head, before.head);
    assert_eq!(h.context.store.thread_head(&id).unwrap(), before.head);
    assert_eq!(
        stored_facts(&h.context, &id).len() as u64,
        before.head.thread_seq
    );
    assert!(
        h.context
            .store
            .receipt(&command_id("send"))
            .unwrap()
            .is_none()
    );
    assert!(h.context.store.outbox(&id).unwrap().is_empty());

    h.context
        .store
        .on_writer(|c| Ok(c.execute_batch("DROP TRIGGER crash_outbox")?))
        .await
        .unwrap();
    let committed = handle
        .dispatch(command_id("send"), send("hello"), CommandOrigin::Client)
        .await
        .unwrap();
    assert!(!committed.replayed);
    assert!(matches!(committed.reply, Reply::Run(_)));
    assert_eq!(
        handle.view().await.unwrap().state.as_ref(),
        &folded(&h.context, &id)
    );
}

#[tokio::test]
async fn coalesces_consecutive_deltas_for_one_item() {
    let h = harness();
    let id = thread("thread:coalesce");
    let handle = created(&h.context, &id).await;
    let (_, attempt) = started_run(&handle, "message").await;
    let before = stored_facts(&h.context, &id).len();
    let mut acks = vec![];
    let (sender, receiver) = mpsc::channel(64);
    for chunk in ["Hel", "lo", ", ", "world"] {
        let (ack, done) = oneshot::channel();
        sender
            .try_send(Mail::Provider {
                attempt: attempt.clone(),
                event: Box::new(ProviderEvent::TextDelta {
                    key: "answer".into(),
                    kind: ProviderItem::Text,
                    text: chunk.into(),
                }),
                ack: Some(ack),
            })
            .ok()
            .unwrap();
        acks.push(done);
    }
    sender
        .try_send(Mail::Provider {
            attempt: attempt.clone(),
            event: Box::new(ProviderEvent::ItemFinished {
                key: "answer".into(),
                kind: ProviderItem::Text,
                text: None,
                status: agent_domain::ItemStatus::Completed,
            }),
            ack: None,
        })
        .ok()
        .unwrap();
    drop(handle);
    let actor = Actor::load(h.context.clone(), id.clone(), receiver)
        .await
        .unwrap();
    let task = tokio::spawn(actor.run());
    let mut results = vec![];
    for done in acks {
        results.push(done.await.unwrap().unwrap());
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]));
    drop(sender);
    task.await.unwrap();
    let appended: Vec<_> = stored_facts(&h.context, &id)[before..]
        .iter()
        .filter_map(|f| match &f.fact.body {
            FactBody::ItemTextAppended { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(appended, ["Hello, world"]);
    let state = folded(&h.context, &id);
    let item = state
        .items
        .iter()
        .find(|i| matches!(i.kind, ItemKind::AssistantMessage { .. }))
        .unwrap();
    assert_eq!(item.text, "Hello, world");
}

#[tokio::test]
async fn does_not_lose_or_duplicate_events_between_catch_up_and_live() {
    let h = harness();
    let id = thread("thread:foundation-stream-race");
    let handle = created(&h.context, &id).await;
    let mut after = handle.view().await.unwrap().head.global_seq;
    for index in 1..=32 {
        let mut subscription = handle
            .subscribe(ThreadSubscribe {
                after_global_seq: Some(after),
                ..ThreadSubscribe::default()
            })
            .await
            .unwrap();
        let written = handle
            .dispatch(
                command_id(&format!("race:{index}")),
                rename(&format!("Race update {index}")),
                CommandOrigin::Client,
            )
            .await
            .unwrap();
        let received = loop {
            match subscription.updates.recv().await.unwrap() {
                ThreadUpdate::Facts(facts) => break facts,
                ThreadUpdate::Synchronized => continue,
                ThreadUpdate::Snapshot(_) => panic!("expected a replay"),
            }
        };
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].global_seq, written.global_seq);
        assert_eq!(
            received[0].fact.body,
            FactBody::ThreadRenamed {
                title: format!("Race update {index}")
            }
        );
        after = received[0].global_seq;
    }
}

#[tokio::test]
async fn subscribe_replays_the_gap_or_sends_a_snapshot() {
    let h = harness();
    let id = thread("thread:subscribe");
    let handle = created(&h.context, &id).await;
    let start = handle.view().await.unwrap().head.global_seq;
    for step in 0..3 {
        handle
            .dispatch(
                command_id(&format!("r{step}")),
                rename(&format!("{step}")),
                CommandOrigin::Client,
            )
            .await
            .unwrap();
    }
    let mut replay = handle
        .subscribe(ThreadSubscribe {
            after_global_seq: Some(start),
            request_completion_marker: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let ThreadUpdate::Facts(facts) = replay.updates.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(facts.len(), 3);
    assert!(matches!(
        replay.updates.recv().await.unwrap(),
        ThreadUpdate::Synchronized
    ));

    let mut fresh = handle
        .subscribe(ThreadSubscribe {
            request_completion_marker: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let ThreadUpdate::Snapshot(view) = fresh.updates.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(view.state.thread.as_ref().unwrap().title, "2");
    assert!(matches!(
        fresh.updates.recv().await.unwrap(),
        ThreadUpdate::Synchronized
    ));

    let mut ahead = handle
        .subscribe(ThreadSubscribe {
            after_global_seq: Some(10_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(matches!(
        ahead.updates.recv().await.unwrap(),
        ThreadUpdate::Snapshot(_)
    ));
}

#[tokio::test]
async fn closes_a_subscriber_that_falls_behind() {
    let h = harness();
    let id = thread("thread:slow");
    let handle = created(&h.context, &id).await;
    let mut slow = handle
        .subscribe(ThreadSubscribe {
            capacity: 4,
            ..ThreadSubscribe::default()
        })
        .await
        .unwrap();
    for step in 0..6 {
        handle
            .dispatch(
                command_id(&format!("r{step}")),
                rename(&format!("{step}")),
                CommandOrigin::Client,
            )
            .await
            .unwrap();
    }
    let mut received = 0;
    while slow.updates.recv().await.is_some() {
        received += 1;
    }
    assert_eq!(received, 4);
    // T3 LiveStreamBudget ends the stream with LiveStreamBufferError.
    assert!(slow.updates.overflowed());
}

// T3 LiveStreamBudget.ts: retained serialized bytes close a stream regardless of count.
#[tokio::test]
async fn closes_a_subscriber_whose_undelivered_facts_exceed_the_byte_budget() {
    let h = harness();
    let id = thread("thread:slow-bytes");
    let handle = created(&h.context, &id).await;
    let budget = ThreadSubscribe {
        max_bytes: 5_000,
        ..ThreadSubscribe::default()
    };
    let mut slow = handle.subscribe(budget).await.unwrap();
    let mut fast = handle.subscribe(budget).await.unwrap();
    assert!(matches!(
        fast.updates.try_recv().unwrap(),
        ThreadUpdate::Snapshot(_)
    ));
    assert!(matches!(
        slow.updates.recv().await.unwrap(),
        ThreadUpdate::Snapshot(_)
    ));
    let mut fast_received = 0;
    for step in 0..6 {
        handle
            .dispatch(
                command_id(&format!("wide{step}")),
                rename(&format!("{step}{}", "x".repeat(2_000))),
                CommandOrigin::Client,
            )
            .await
            .unwrap();
        let update = fast.updates.try_recv().unwrap();
        assert!(update.live_bytes() > 2_000);
        fast_received += 1;
    }
    assert_eq!(fast_received, 6);
    assert_eq!(
        fast.updates.try_recv().unwrap_err(),
        tokio::sync::mpsc::error::TryRecvError::Empty
    );
    let mut received = 0;
    while slow.updates.recv().await.is_some() {
        received += 1;
    }
    assert_eq!(received, 2);
}

/// Claims rows as `worker` until the thread's provider start is running under its lease.
async fn claim_start(h: &Harness, id: &ThreadId, worker: &str) -> (Arc<SqliteOutbox>, String) {
    let outbox = SqliteOutbox::new(h.context.store.clone(), h.clock.clone());
    loop {
        let row = outbox
            .claim(worker, Duration::from_secs(30))
            .await
            .unwrap()
            .expect("the provider start is claimable");
        if &row.thread == id
            && matches!(
                row.effect.body,
                EffectBody::Provider(ProviderCommand::Start { .. })
            )
        {
            return (outbox, row.effect.id);
        }
    }
}
fn start_failed(attempt: RunAttemptId) -> EffectResult {
    EffectResult::ProviderFailed {
        session_lost: false,
        attempt,
        operation: ProviderOperation::Start,
        message: "spawn failed".into(),
        message_id: None,
        turn_completed: false,
    }
}

#[tokio::test]
async fn settles_the_consumed_effect_in_the_same_commit() {
    let h = harness();
    let id = thread("thread:settle");
    let handle = created(&h.context, &id).await;
    let (run, attempt) = started_run(&handle, "message").await;
    let (_outbox, start) = claim_start(&h, &id, "worker").await;
    let start = h
        .context
        .store
        .outbox(&id)
        .unwrap()
        .into_iter()
        .find(|row| row.effect.id == start)
        .unwrap();
    let committed = handle
        .effect_result(
            start.effect.id.clone(),
            "worker".into(),
            start_failed(attempt),
        )
        .await
        .unwrap();
    assert!(!committed.replayed);
    let row = h
        .context
        .store
        .outbox(&id)
        .unwrap()
        .into_iter()
        .find(|row| row.effect.id == start.effect.id)
        .unwrap();
    assert_eq!(row.status, crate::EffectStatus::Succeeded);
    let view = handle.view().await.unwrap();
    assert_eq!(
        view.state.runs.iter().find(|r| r.id == run).unwrap().status,
        RunStatus::Failed
    );
}

#[tokio::test]
async fn commits_no_result_of_an_effect_cancelled_while_it_ran() {
    let h = harness();
    let id = thread("thread:settle-cancelled");
    let handle = created(&h.context, &id).await;
    let (run, attempt) = started_run(&handle, "message").await;
    let (outbox, start) = claim_start(&h, &id, "worker").await;
    let cancelled = outbox
        .cancel(&id, &["Provider.Start"], "superseded")
        .await
        .unwrap();
    assert_eq!(cancelled, std::slice::from_ref(&start));
    let before = stored_facts(&h.context, &id).len();

    let error = handle
        .effect_result(start.clone(), "worker".into(), start_failed(attempt))
        .await
        .unwrap_err();

    assert!(
        matches!(&error, RuntimeError::Store(store) if matches!(**store, StoreError::NotLeased(_))),
        "{error:?}"
    );
    assert_eq!(stored_facts(&h.context, &id).len(), before);
    let view = handle.view().await.unwrap();
    assert_eq!(
        view.state.runs.iter().find(|r| r.id == run).unwrap().status,
        RunStatus::Running
    );
    let row = h.context.store.outbox(&id).unwrap();
    let row = row.iter().find(|row| row.effect.id == start).unwrap();
    assert_eq!(row.status, crate::EffectStatus::Cancelled);
}

#[tokio::test]
async fn commits_no_result_from_a_worker_that_does_not_hold_the_lease() {
    let h = harness();
    let id = thread("thread:settle-foreign");
    let handle = created(&h.context, &id).await;
    let (run, attempt) = started_run(&handle, "message").await;
    let (_outbox, start) = claim_start(&h, &id, "owner").await;
    let before = stored_facts(&h.context, &id).len();

    assert!(
        handle
            .effect_result(start.clone(), "other".into(), start_failed(attempt.clone()))
            .await
            .is_err()
    );
    assert_eq!(stored_facts(&h.context, &id).len(), before);
    let row = h.context.store.outbox(&id).unwrap();
    let row = row.iter().find(|row| row.effect.id == start).unwrap();
    assert_eq!(row.status, crate::EffectStatus::Running);
    assert_eq!(row.lease_owner.as_deref(), Some("owner"));

    handle
        .effect_result(start, "owner".into(), start_failed(attempt))
        .await
        .unwrap();
    let view = handle.view().await.unwrap();
    assert_eq!(
        view.state.runs.iter().find(|r| r.id == run).unwrap().status,
        RunStatus::Failed
    );
}

#[tokio::test]
async fn timestamps_never_move_backwards_and_snoozes_wake_on_time() {
    let h = harness();
    let id = thread("thread:timer");
    let handle = created(&h.context, &id).await;
    h.clock.advance(-60_000);
    handle
        .dispatch(command_id("rename"), rename("later"), CommandOrigin::Client)
        .await
        .unwrap();
    let facts = stored_facts(&h.context, &id);
    assert!(facts.windows(2).all(|w| w[0].fact.at <= w[1].fact.at));
    assert_eq!(facts.last().unwrap().fact.at, at());

    h.clock.set(&at());
    let until = Timestamp::from_millis(at().millis() + 30).unwrap();
    handle
        .dispatch(
            command_id("snooze"),
            Command::Snooze { until: Some(until) },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    let mut updates = handle
        .subscribe(ThreadSubscribe::default())
        .await
        .unwrap()
        .updates;
    h.clock.advance(30);
    let woke = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let ThreadUpdate::Facts(facts) = updates.recv().await.unwrap()
                && facts
                    .iter()
                    .any(|f| f.fact.body == FactBody::ThreadSnoozed { until: None })
            {
                return;
            }
        }
    })
    .await;
    assert!(woke.is_ok());
    assert!(
        handle
            .view()
            .await
            .unwrap()
            .state
            .thread
            .as_ref()
            .unwrap()
            .snoozed_until
            .is_none()
    );
}

struct Catalog;
impl HandoffCatalog for Catalog {
    fn policy(&self, selection: &ModelSelection) -> Option<HandoffValue> {
        (selection.model == "gpt-6-luna").then_some(HandoffValue {
            model_window: Some(200_000),
            token_cap: 16_000,
        })
    }
}

// T3 ContextHandoffBudget: the default token cap, and the window only the
// Claude catalog knows (ClaudeAdapterV2 getModelContextWindow).
#[test]
fn the_provider_catalog_has_the_reference_handoff_limits() {
    let selection = |driver, model: &str, window: Option<&str>| ModelSelection {
        instance: format!("{driver:?}"),
        driver,
        model: model.into(),
        options: window
            .map(|window| ("contextWindow".to_owned(), window.to_owned()))
            .into_iter()
            .collect(),
    };
    let policy = |driver, model, window| {
        crate::ProviderHandoffCatalog
            .policy(&selection(driver, model, window))
            .unwrap()
    };
    assert_eq!(
        policy(agent_domain::Driver::Codex, "gpt-6-luna", None),
        HandoffValue {
            model_window: None,
            token_cap: 16_000
        }
    );
    assert_eq!(
        policy(agent_domain::Driver::Claude, "claude-opus-4-8", None).model_window,
        Some(1_000_000)
    );
    assert_eq!(
        policy(agent_domain::Driver::Claude, "claude-sonnet-4-6", None).model_window,
        Some(200_000)
    );
    assert_eq!(
        policy(agent_domain::Driver::Claude, "claude-fable-5", Some("200k")).model_window,
        Some(200_000)
    );
    assert_eq!(
        policy(agent_domain::Driver::Claude, "claude-haiku-4-5", None).model_window,
        None
    );
}

#[tokio::test]
async fn queues_a_handoff_policy_when_the_catalog_differs() {
    let mut h = harness();
    h.context.handoff = Arc::new(Catalog);
    let id = thread("thread:handoff");
    let handle = created(&h.context, &id).await;
    let view = handle.view().await.unwrap();
    assert_eq!(view.state.handoff_token_cap, Some(16_000));
    assert_eq!(view.state.context_windows.get("codex"), Some(&200_000));
    handle
        .dispatch(command_id("rename"), rename("x"), CommandOrigin::Client)
        .await
        .unwrap();
    let policies = stored_facts(&h.context, &id)
        .iter()
        .filter(|f| matches!(f.fact.body, FactBody::HandoffPolicyChanged { .. }))
        .count();
    assert_eq!(policies, 1);
}

struct Windows;
impl HandoffCatalog for Windows {
    fn policy(&self, selection: &ModelSelection) -> Option<HandoffValue> {
        Some(HandoffValue {
            model_window: Some(if selection.model == "wide" {
                1_000_000
            } else {
                200_000
            }),
            token_cap: 16_000,
        })
    }
}

// T3 ProviderTurnStartService reads the selected run's model window before
// budgeting delivery, so a message that selects another window starts with it.
#[tokio::test]
async fn a_run_starts_with_the_model_window_its_message_selects() {
    let mut h = harness();
    h.context.handoff = Arc::new(Windows);
    let id = thread("thread:window");
    let handle = created(&h.context, &id).await;
    let Command::Send(mut message) = send("wide") else {
        unreachable!()
    };
    message.selection = Some(ModelSelection {
        model: "wide".into(),
        ..selection()
    });
    let sent = handle
        .dispatch(
            command_id("wide"),
            Command::Send(message),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert!(matches!(sent.reply, Reply::Run(_)));
    let facts: Vec<_> = stored_facts(&h.context, &id)
        .into_iter()
        .map(|stored| stored.fact)
        .collect();
    let started = facts
        .iter()
        .position(|fact| matches!(fact.body, FactBody::RunStarted { .. }))
        .unwrap();
    let before = fold(&State::default(), &facts[..started]).unwrap();
    assert_eq!(before.context_windows.get("codex"), Some(&1_000_000));
}

struct Pinned(Mutex<bool>);
impl Residency for Pinned {
    fn pinned(&self, _: &ThreadId) -> bool {
        *self.0.lock().unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registry_loads_each_thread_once_and_evicts_idle_actors() {
    let mut h = harness();
    let pin = Arc::new(Pinned(Mutex::new(false)));
    h.context.residency = pin.clone();
    let registry = ActorRegistry::new(h.context.clone());
    let id = thread("thread:registry");
    let loads: Vec<_> = (0..16)
        .map(|_| {
            let (registry, id) = (registry.clone(), id.clone());
            tokio::spawn(async move { registry.get_or_load(&id).await.unwrap() })
        })
        .collect();
    let mut handles = vec![];
    for load in loads {
        handles.push(load.await.unwrap());
    }
    assert!(
        handles
            .windows(2)
            .all(|w| w[0].mail.same_channel(&w[1].mail))
    );
    registry
        .dispatch(
            &id,
            command_id("create"),
            create(&id),
            CommandOrigin::Client,
        )
        .await
        .unwrap();

    let subscription = handles[0]
        .subscribe(ThreadSubscribe::default())
        .await
        .unwrap();
    assert!(registry.evict_idle(Duration::ZERO).await.is_empty());
    drop(subscription);
    *pin.0.lock().unwrap() = true;
    assert!(registry.evict_idle(Duration::ZERO).await.is_empty());
    *pin.0.lock().unwrap() = false;
    assert!(
        registry
            .evict_idle(Duration::from_secs(3600))
            .await
            .is_empty()
    );
    assert_eq!(
        registry.evict_idle(Duration::ZERO).await,
        std::slice::from_ref(&id)
    );
    assert!(handles[0].is_closed());
    assert!(registry.loaded().is_empty());

    let stale = handles.pop().unwrap();
    assert!(matches!(
        stale
            .dispatch(command_id("stale"), rename("x"), CommandOrigin::Client)
            .await,
        Err(RuntimeError::ActorStopped)
    ));
    let renamed = registry
        .dispatch(
            &id,
            command_id("after-evict"),
            rename("after"),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert_eq!(renamed.reply, Reply::Accepted);
    assert_eq!(renamed.thread_seq, 2);

    let busy = thread("thread:busy");
    registry
        .dispatch(
            &busy,
            command_id("busy:create"),
            create(&busy),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    registry
        .dispatch(
            &busy,
            command_id("busy:send"),
            send("m"),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    let evicted = registry.evict_idle(Duration::ZERO).await;
    assert_eq!(evicted, [id], "open outbox rows keep {busy} loaded");
}
