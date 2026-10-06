use super::*;

// T3 Orchestrator.ts dispatchQueuedMessagePromoteToSteer / dispatchSteerIntoRun:
// native maintenance never joins a running turn and a maintenance turn takes
// no steering.
#[test]
fn promoting_to_steer_keeps_maintenance_separate() {
    let mut s = state();
    let (active, _) = running(&mut s, "work");
    let compact = queued(&mut s, "/compact");
    let promote = |s: &mut State, key: &str, queued: &RunId, active: &RunId| {
        command(
            s,
            key,
            Command::PromoteToSteer {
                queued: queued.clone(),
                active: active.clone(),
            },
        )
        .reply
    };
    assert_eq!(
        promote(&mut s, "promote-compact", &compact, &active),
        Reply::Rejected {
            reason: "maintenance-must-run-separately".into()
        }
    );
    let mut busy = state();
    let maintenance = {
        let Reply::Run(run) = command(
            &mut busy,
            "/compact",
            send_message("/compact", DispatchMode::StartImmediately),
        )
        .reply
        else {
            panic!()
        };
        let attempt = busy.runs[0].attempt.clone().unwrap();
        provider(
            &mut busy,
            "started",
            &attempt,
            ProviderEvent::TurnStarted { native_turn: None },
        );
        run
    };
    let follow_up = queued(&mut busy, "follow-up");
    assert_eq!(
        promote(&mut busy, "promote", &follow_up, &maintenance),
        Reply::Rejected {
            reason: "maintenance-in-progress".into()
        }
    );
    assert_eq!(busy.runs[1].status, RunStatus::Queued);
}

fn wake(s: &mut State, attempt: &RunAttemptId, key: &str) -> RunId {
    provider(
        s,
        &format!("{key}-report"),
        attempt,
        ProviderEvent::BackgroundTask {
            key: key.into(),
            tool: "Bash".into(),
            kind: BackgroundKind::Command,
            description: key.into(),
            status: Some(ItemStatus::Completed),
            summary: Some("done".into()),
            exit_code: Some(0),
        },
    );
    provider(
        s,
        key,
        attempt,
        ProviderEvent::Wake {
            text: key.into(),
            detail: None,
        },
    );
    let run = s.runs.last().unwrap();
    assert!(s.message(&run.message).unwrap().notification.is_some());
    run.id.clone()
}
fn queued(s: &mut State, key: &str) -> RunId {
    let Reply::Run(run) = command(s, key, send_message(key, DispatchMode::QueueAfterActive)).reply
    else {
        panic!()
    };
    run
}
fn order(s: &State) -> Vec<RunId> {
    s.queued_runs().iter().map(|run| run.id.clone()).collect()
}

// T3 runtimeLayer.test.ts "starts a wake's work clock from the run that ran
// before it": a background wake keeps its queue position and a queued message
// can be reordered behind it.
#[test]
fn background_wakes_keep_their_queue_position_and_can_be_passed() {
    let mut s = state();
    let (_, attempt) = running(&mut s, "prompt");
    let first = queued(&mut s, "queued");
    let early = wake(&mut s, &attempt, "early-wake");
    assert_eq!(order(&s), [first.clone(), early.clone()]);
    assert_eq!(
        command(
            &mut s,
            "reorder",
            Command::ReorderQueued {
                run: first.clone(),
                before: None,
            },
        )
        .reply,
        Reply::Accepted
    );
    let late = wake(&mut s, &attempt, "late-wake");
    assert_eq!(order(&s), [early, first, late]);
}

// T3 Orchestrator.ts dispatchQueuedRunReorder.
#[test]
fn reordering_follows_the_reference_rules() {
    let mut s = state();
    let (_, attempt) = running(&mut s, "prompt");
    let first = queued(&mut s, "first");
    let second = queued(&mut s, "second");
    let notice = wake(&mut s, &attempt, "notice");
    let reject = |reason: &str| Reply::Rejected {
        reason: reason.into(),
    };
    assert_eq!(
        command(
            &mut s,
            "move-notice",
            Command::ReorderQueued {
                run: notice.clone(),
                before: None,
            },
        )
        .reply,
        reject("automatic-delivery-not-reorderable")
    );
    assert_eq!(
        command(
            &mut s,
            "onto-self",
            Command::ReorderQueued {
                run: second.clone(),
                before: Some(second.clone()),
            },
        )
        .reply,
        reject("queued-run-not-found")
    );
    assert_eq!(
        command(
            &mut s,
            "ahead-of-notice",
            Command::ReorderQueued {
                run: second.clone(),
                before: Some(first.clone()),
            },
        )
        .reply,
        Reply::Accepted
    );
    assert_eq!(order(&s), [second.clone(), first.clone(), notice.clone()]);
    assert_eq!(
        command(
            &mut s,
            "before-notice",
            Command::ReorderQueued {
                run: second.clone(),
                before: Some(notice.clone()),
            },
        )
        .reply,
        Reply::Accepted
    );
    assert_eq!(order(&s), [first, second, notice]);
}

pub(super) fn delegate(s: &mut State, id: &str) -> NodeId {
    let task = NodeId::new(id).unwrap();
    command(
        s,
        &format!("delegate-{id}"),
        Command::Delegate {
            task: task.clone(),
            child: ThreadId::new(format!("child-{id}")).unwrap(),
            prompt: id.into(),
            title: None,
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            wake: CompletionWake::Always,
        },
    );
    task
}
fn complete(s: &mut State, task: &NodeId) -> Step {
    let source_message = s
        .tasks
        .iter()
        .find(|candidate| &candidate.id == task)
        .and_then(|task| task.original_message.clone());
    command(
        s,
        &format!("finish-{task}"),
        Command::TaskResult {
            generation: None,
            source_message,
            context: None,
            task: task.clone(),
            status: ItemStatus::Completed,
            result: "Done".into(),
        },
    )
}

// T3 Orchestrator.ts dispatchQueuedRunCancel: cancelling a queued delegated
// completion disposes the parent run's whole cohort, so a sibling that is
// still running never wakes the parent.
#[test]
fn cancelling_a_completion_delivery_disposes_running_siblings() {
    let mut s = state();
    let (parent, _) = starting(&mut s, "parent");
    let done = delegate(&mut s, "done");
    let running = delegate(&mut s, "running");
    complete(&mut s, &done);
    let Reply::Run(delivery) = command(
        &mut s,
        "wake",
        Command::AcceptTaskWake {
            task_ids: vec![done.clone()],
        },
    )
    .reply
    else {
        panic!()
    };
    command(&mut s, "cancel", Command::CancelQueued { run: delivery });
    for task in &s.tasks {
        assert_eq!(task.delivery, DeliveryState::Disposed, "{}", task.id);
        assert_eq!(task.run.as_ref(), Some(&parent));
    }
    let finished = complete(&mut s, &running);
    assert!(!finished.effects.iter().any(
        |effect| matches!(&effect.body, EffectBody::SendToThread { command, .. }
                if matches!(command.as_ref(), Command::AcceptTaskWake { .. }))
    ));
}

// T3 Orchestrator.ts dispatchRunInterrupt: Stop disposes the run's cohort but
// leaves delegated children running in their own threads.
#[test]
fn stopping_the_parent_leaves_delegated_children_running() {
    let mut s = state();
    let (parent, _) = running(&mut s, "parent");
    let child = delegate(&mut s, "child");
    let stop = command(
        &mut s,
        "stop",
        Command::Interrupt {
            run: parent,
            hold_queue: true,
            reason: None,
        },
    );
    assert!(!stop.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::SendToThread { command, .. } if matches!(command.as_ref(), Command::Stop)
    )));
    let task = s.tasks.iter().find(|task| task.id == child).unwrap();
    assert_eq!(task.status, ItemStatus::Running);
    assert_eq!(task.delivery, DeliveryState::Disposed);
}
