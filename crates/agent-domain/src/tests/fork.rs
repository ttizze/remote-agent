use super::*;

fn fork(s: &mut State, key: &str, source: SourcePoint) -> Step {
    command(
        s,
        key,
        Command::Fork {
            target: ThreadId::new(format!("{key}-child")).unwrap(),
            source,
            title: None,
            created_by: crate::MessageAuthor::User,
            creation_source: "desktop".into(),
        },
    )
}
fn start_of(step: &Step) -> Option<(Option<String>, Option<HistoricalContext>)> {
    step.effects.iter().find_map(|effect| match &effect.body {
        EffectBody::Provider(ProviderCommand::Start {
            native_thread,
            context,
            ..
        }) => Some((native_thread.clone(), context.clone())),
        _ => None,
    })
}
fn native_fork(step: &Step) -> Option<(RunAttemptId, String, Option<String>)> {
    step.effects.iter().find_map(|effect| match &effect.body {
        EffectBody::ForkNative {
            provider:
                ProviderCommand::Fork {
                    native_thread,
                    through_turn,
                },
            ..
        } => Some((
            effect.attempt.clone().unwrap(),
            native_thread.clone(),
            through_turn.clone(),
        )),
        _ => None,
    })
}
fn completed(s: &mut State, key: &str, text: &str) -> RunId {
    let (run, attempt) = running(s, key);
    provider(
        s,
        &format!("{key}-text"),
        &attempt,
        ProviderEvent::TextDelta {
            key: format!("{key}-text"),
            kind: ProviderItem::Text,
            text: text.into(),
        },
    );
    finish(s, &attempt);
    run
}

// The fork creates the child at once and the child's first message forks
// natively on the source's provider; the inherited history is fixed.
#[test]
fn a_native_fork_happens_when_the_child_sends_its_first_message() {
    let mut s = state();
    let run = completed(&mut s, "first", "fork marker");
    let step = fork(&mut s, "fork", SourcePoint::Run(run));
    assert!(native_fork(&step).is_none());
    let mut child = accept_child(&step);
    let history = child.inherited_items.clone();
    command(
        &mut s,
        "rename",
        Command::Rename {
            title: "Changed parent".into(),
        },
    );
    let start = command(
        &mut child,
        "child-send",
        send_message("child-send", DispatchMode::StartImmediately),
    );
    assert!(start_of(&start).is_none());
    let (attempt, native, head) = native_fork(&start).unwrap();
    assert_eq!(native, "native-thread");
    assert_eq!(head.as_deref(), Some("native-head"));
    let forked = result(
        &mut child,
        "forked",
        EffectResult::NativeForked {
            attempt,
            native_thread: "fork-native".into(),
        },
    );
    let (native_thread, context) = start_of(&forked).unwrap();
    assert_eq!(native_thread.as_deref(), Some("fork-native"));
    assert!(context.is_none());
    assert_eq!(
        child.transfers[0].delivery.as_ref().unwrap().status,
        ContextDeliveryStatus::NativeFork
    );
    assert_eq!(child.inherited_items, history);
    assert!(
        child
            .inherited_items
            .iter()
            .any(|i| i.text == "fork marker")
    );
    assert_eq!(child.thread.unwrap().title, "Thread fork");
}

// Resolves a pending fork against the first message's own selection: on
// another provider the fork is portable (full_thread_summary).
#[test]
fn a_fork_switched_to_another_provider_hands_over_its_history() {
    let mut s = state();
    let run = completed(&mut s, "first", "inherited answer");
    let mut child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(run)));
    command(
        &mut child,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let start = command(
        &mut child,
        "child-send",
        send_message("child-send", DispatchMode::StartImmediately),
    );
    assert!(native_fork(&start).is_none());
    let (native_thread, context) = start_of(&start).unwrap();
    assert_eq!(native_thread, None);
    let context = context.unwrap();
    assert!(render_history(&context).contains("inherited answer"));
    assert!(context.context.contains("full_thread_summary"));
    assert_eq!(child.transfers[0].instance.as_deref(), Some("claude"));
}

// A native fork that fails fails the run, and the pending fork is resolved
// again by the next message.
#[test]
fn a_failed_native_fork_fails_the_run_and_the_next_message_retries_it() {
    let mut s = state();
    let run = completed(&mut s, "source", "source answer");
    let mut child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(run)));
    let start = command(
        &mut child,
        "first",
        send_message("first", DispatchMode::StartImmediately),
    );
    let (attempt, ..) = native_fork(&start).unwrap();
    result(
        &mut child,
        "fork-failed",
        EffectResult::ForkFailed {
            attempt: attempt.clone(),
            message: "transcript unavailable".into(),
        },
    );
    assert_eq!(child.runs[0].status, RunStatus::Failed);
    assert!(child.items.iter().any(|item| matches!(&item.kind,
        ItemKind::Error { message, .. } if message == "transcript unavailable")));
    assert!(child.transfers[0].delivery.is_none());
    assert_eq!(
        result(
            &mut child,
            "late",
            EffectResult::NativeForked {
                attempt,
                native_thread: "late".into(),
            },
        )
        .reply,
        Reply::Ignored
    );
    let retry = command(
        &mut child,
        "retry",
        send_message("retry", DispatchMode::StartImmediately),
    );
    assert!(native_fork(&retry).is_some());
}

// ThreadForkService.ts copies subagent items with their prompt, progress and
// result: a fork, and a fork of that fork, keep the task as it was at the fork.
#[test]
fn a_fork_keeps_its_subagent_tasks_as_they_were_at_the_fork() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    provider(
        &mut s,
        "spawn",
        &a,
        ProviderEvent::SubagentStarted {
            background: true,
            native_thread: None,
            key: "child".into(),
            parent: None,
            prompt: "Inspect code".into(),
            model: None,
        },
    );
    provider(
        &mut s,
        "progress",
        &a,
        ProviderEvent::SubagentProgress {
            key: "child".into(),
            progress: "Reading files".into(),
            model: None,
        },
    );
    finish(&mut s, &a);
    let at_fork = s.tasks[0].clone();
    let mut child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(run)));
    assert!(
        child
            .inherited_items
            .iter()
            .any(|item| matches!(&item.kind, ItemKind::Subagent { task } if task == &at_fork.id))
    );
    assert_eq!(child.inherited_tasks, std::slice::from_ref(&at_fork));
    let child_run = completed(&mut child, "child request", "child answer");
    let grandchild = accept_child(&fork(&mut child, "fork-again", SourcePoint::Run(child_run)));
    assert_eq!(grandchild.inherited_tasks, [at_fork]);
}

// A fork of a fork keeps the inherited prefix and its message fields.
#[test]
fn forking_a_fork_keeps_the_ancestor_conversation_and_its_messages() {
    let mut s = state();
    let mut send = send_message("ancestor request", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut send {
        message.attachments = vec![captured_image()];
    }
    command(&mut s, "ancestor request", send);
    let a = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "started",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &a);
    let run = s.runs[0].id.clone();
    let mut child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(run)));
    let child_run = completed(&mut child, "child request", "child answer");
    let grandchild = accept_child(&fork(&mut child, "fork-again", SourcePoint::Run(child_run)));
    let texts = grandchild
        .visible_items()
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::UserMessage { .. }))
        .map(|item| item.text.clone())
        .collect::<Vec<_>>();
    assert_eq!(texts, ["ancestor request", "child request"]);
    assert_eq!(
        grandchild
            .visible_items()
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Fork { .. }))
            .count(),
        2
    );
    let ItemKind::UserMessage { message } = &grandchild.inherited_items[0].kind else {
        panic!()
    };
    let message = grandchild.message(message).unwrap();
    assert_eq!(message.attachments, vec![captured_image()]);
    assert_eq!(message.intent, InputIntent::TurnStart);
    assert_eq!(message.created_by, MessageAuthor::User);
    let Some(Transfer { history, .. }) = grandchild.transfers.first() else {
        panic!()
    };
    assert!(render_history(history).contains("ancestor request"));
}

// Unsuccessful sources fork from the bounded portable history.
#[test]
fn unsuccessful_and_cancelled_runs_fork_from_bounded_portable_history() {
    let mut s = state();
    let (failed, a) = running(&mut s, "failed request");
    provider(
        &mut s,
        "failed",
        &a,
        ProviderEvent::TurnFinished {
            status: RunStatus::Failed,
            native_head: None,
        },
    );
    completed(&mut s, "later", "later answer");
    let (active, _) = running(&mut s, "still running");
    let mut child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(failed)));
    let history = render_history(&child.transfers[0].history);
    assert!(history.contains("failed request") && !history.contains("later answer"));
    assert_eq!(child.transfers[0].native_source, None);
    let start = command(
        &mut child,
        "child-send",
        send_message("child-send", DispatchMode::StartImmediately),
    );
    assert!(native_fork(&start).is_none());
    assert!(start_of(&start).unwrap().1.is_some());
    assert_eq!(
        fork(&mut s, "fork-running", SourcePoint::Run(active)).reply,
        Reply::Rejected {
            reason: "fork-source-not-ready".into()
        }
    );
    let Reply::Run(queued) = command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    )
    .reply
    else {
        panic!()
    };
    command(
        &mut s,
        "cancel",
        Command::CancelQueued {
            run: queued.clone(),
        },
    );
    assert!(matches!(
        fork(&mut s, "fork-cancelled", SourcePoint::Run(queued)).reply,
        Reply::Thread(_)
    ));
}

// A fork inherits every item through its boundary run, including runs a
// rollback discarded.
#[test]
fn a_fork_inherits_rolled_back_runs_through_its_boundary() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (second, b) = running(&mut s, "discarded request");
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: cp,
            restore_files: false,
            restore_refusal: None,
        },
    );
    result(
        &mut s,
        "rolled-back",
        EffectResult::RollbackFinished {
            command: CommandId::new("rollback").unwrap(),
            bindings: vec![],
        },
    );
    assert_eq!(s.runs[1].status, RunStatus::RolledBack);
    let (third, c) = running(&mut s, "third");
    finish(&mut s, &c);
    checkpoint(&mut s, &third, &c, "cp-third");
    let child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(third)));
    assert!(
        child
            .inherited_items
            .iter()
            .any(|item| item.text == "discarded request")
    );
}

#[test]
fn fork_sources_follow_the_reference_source_points() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (_, b) = running(&mut s, "uncaptured");
    finish(&mut s, &b);
    let latest = accept_child(&fork(&mut s, "latest", SourcePoint::LatestStable));
    assert_eq!(latest.thread.as_ref().unwrap().fork_boundary, Some(1));
    let by_checkpoint = accept_child(&fork(&mut s, "checkpoint", SourcePoint::Checkpoint(cp)));
    assert_eq!(
        by_checkpoint.thread.as_ref().unwrap().fork_boundary,
        Some(1)
    );
    let mut empty = state();
    assert_eq!(
        fork(&mut empty, "none", SourcePoint::LatestStable).reply,
        Reply::Rejected {
            reason: "no-stable-source-run".into()
        }
    );
    let titled = command(
        &mut s,
        "titled",
        Command::Fork {
            target: ThreadId::new("titled-child").unwrap(),
            source: SourcePoint::Run(first.clone()),
            title: Some("  Named fork ".into()),
            created_by: crate::MessageAuthor::User,
            creation_source: "desktop".into(),
        },
    );
    assert_eq!(accept_child(&titled).thread.unwrap().title, "Named fork");
    assert_eq!(
        command(
            &mut s,
            "blank",
            Command::Fork {
                target: ThreadId::new("blank-child").unwrap(),
                source: SourcePoint::Run(first),
                title: Some(" ".into()),
                created_by: crate::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        )
        .reply,
        Reply::Rejected {
            reason: "title-required".into()
        }
    );
}

// The fork copies the parent row, including its pin and automatic
// settlement.
#[test]
fn a_fork_copies_the_parent_sidebar_arrangement() {
    let mut s = state();
    command(
        &mut s,
        "pin",
        Command::Pin {
            pinned: true,
            order: Some("a0".into()),
        },
    );
    command(&mut s, "manual", Command::AutoSettle { enabled: false });
    let run = completed(&mut s, "first", "answer");
    let child = accept_child(&fork(&mut s, "fork", SourcePoint::Run(run)));
    let thread = child.thread.unwrap();
    let parent = s.thread.unwrap();
    assert_eq!(thread.pinned_at, parent.pinned_at);
    assert_eq!(thread.pin_order.as_deref(), Some("a0"));
    assert!(!thread.auto_settle);
}

fn merged(s: &mut State, child: &mut State, key: &str, source: SourcePoint) -> Reply {
    let step = command(
        child,
        key,
        Command::MergeBack {
            target: ThreadId::new("thread").unwrap(),
            source,
        },
    );
    let reply = step.reply.clone();
    for effect in step.effects {
        if let EffectBody::SendToThread {
            command: accept, ..
        } = effect.body
        {
            command(s, &format!("{key}-accept"), *accept);
        }
    }
    reply
}

// Merge-back takes the latest stable fork run, is delivered by the next
// direct turn with whatever provider it uses, and queued messages are
// rejected while it is pending.
#[test]
fn merge_back_waits_for_a_direct_turn_on_any_provider() {
    let mut parent = state();
    let run = completed(&mut parent, "parent-marker", "parent answer");
    let mut child = accept_child(&fork(&mut parent, "fork", SourcePoint::Run(run)));
    command(
        &mut child,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    assert_eq!(
        merged(
            &mut parent,
            &mut child,
            "too-early",
            SourcePoint::LatestStable
        ),
        Reply::Rejected {
            reason: "no-stable-source-run".into()
        }
    );
    let child_run = completed(&mut child, "child-marker", "child answer");
    let attempt = child.runs[0].attempt.clone().unwrap();
    checkpoint(&mut child, &child_run, &attempt, "cp-child");
    let (_, active) = running(&mut parent, "busy");
    assert_eq!(
        merged(&mut parent, &mut child, "merge", SourcePoint::LatestStable),
        Reply::Accepted
    );
    assert_eq!(parent.transfers.len(), 1);
    let history = render_history(&parent.transfers[0].history);
    assert!(history.contains("child-marker") && !history.contains("parent-marker"));
    assert_eq!(
        command(
            &mut parent,
            "queued",
            send_message("queued", DispatchMode::QueueAfterActive),
        )
        .reply,
        Reply::Rejected {
            reason: "merge-back-pending".into()
        }
    );
    finish(&mut parent, &active);
    command(
        &mut parent,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let start = command(
        &mut parent,
        "after-merge",
        send_message("after-merge", DispatchMode::StartImmediately),
    );
    let (_, context) = start_of(&start).unwrap();
    let context = context.unwrap();
    assert!(render_history(&context).contains("child-marker"));
    assert!(context.context.contains("merge_back / fork_delta_summary"));
    assert_eq!(parent.transfers[0].instance.as_deref(), Some("claude"));
}

// Selects only handoffs for the starting run or carried from a failed or
// interrupted run on the same provider thread; a delegated result is handed
// to its spawning run.
#[test]
fn delegated_results_reach_a_later_turn_only_after_the_spawning_run_failed() {
    for (spawning_status, delivered) in [(RunStatus::Completed, false), (RunStatus::Failed, true)] {
        let mut s = state();
        let (_, attempt) = running(&mut s, "parent");
        let task = queue::delegate(&mut s, "child");
        provider(
            &mut s,
            "parent-end",
            &attempt,
            ProviderEvent::TurnFinished {
                status: spawning_status,
                native_head: None,
            },
        );
        let source_message = s.tasks[0].original_message.clone();
        command(
            &mut s,
            "result",
            Command::TaskResult {
                source_message,
                generation: None,
                context: Some(TaskResultContext {
                    boundary: 1,
                    history: HistoricalContext {
                        messages: vec![],
                        context: "Child result".into(),
                        omitted_items: 0,
                        omitted_item_ids: vec![],
                    },
                }),
                task,
                status: ItemStatus::Completed,
                result: "done".into(),
            },
        );
        command(&mut s, "resume", Command::ResumeQueue);
        let next = command(
            &mut s,
            "next",
            send_message("next", DispatchMode::StartImmediately),
        );
        let context = next.effects.iter().find_map(|effect| match &effect.body {
            EffectBody::Provider(ProviderCommand::Start { context, .. }) => Some(context.clone()),
            _ => None,
        });
        assert_eq!(
            context
                .flatten()
                .is_some_and(|context| context.context.contains("manual_context")),
            delivered,
            "{spawning_status:?}"
        );
    }
}
