//! `SendToThread`, including EffectWorker.test.ts "settles a delegated child once
//! its restart continuation fails for good".
use super::*;
use agent_domain::{Effect, EffectBody};

#[derive(Default)]
struct Threads {
    dispatched: Mutex<Vec<(ThreadId, CommandId, Command)>>,
    setting: Mutex<Option<Result<bool, RuntimeError>>>,
}
impl ThreadCommands for Threads {
    fn dispatch(
        &self,
        thread: ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Committed, RuntimeError>> {
        Box::pin(async move {
            self.dispatched.lock().unwrap().push((thread, id, command));
            Ok(Committed {
                reply: Reply::Accepted,
                thread_seq: 1,
                global_seq: 1,
                replayed: false,
            })
        })
    }
    fn continue_after_restart(&self, _: ThreadId) -> BoxFuture<'_, Result<bool, RuntimeError>> {
        Box::pin(async move { self.setting.lock().unwrap().clone().unwrap_or(Ok(true)) })
    }
}

fn job(command: Command, will_retry: bool) -> EffectJob {
    EffectJob {
        effect: Effect {
            id: "effect:send".into(),
            attempt: None,
            body: EffectBody::SendToThread {
                thread: tid("thread:target"),
                command: Box::new(command),
            },
        },
        thread: tid("thread:source"),
        attempt: 1,
        will_retry,
    }
}

fn continuation(enabled: bool) -> Command {
    Command::ContinueRestart {
        source: RunId::new("run:cut").unwrap(),
        enabled,
    }
}

#[tokio::test]
async fn delivers_with_the_effect_command_id_and_the_current_continuation_setting() {
    let threads = Arc::new(Threads::default());
    *threads.setting.lock().unwrap() = Some(Ok(false));
    let handler = SendToThread::new(threads.clone());

    assert_eq!(handler.run(job(continuation(true), true)).await, Ok(None));
    assert_eq!(
        handler
            .run(job(
                Command::Rename {
                    title: "Child".into()
                },
                true
            ))
            .await,
        Ok(None)
    );
    let dispatched = threads.dispatched.lock().unwrap().clone();
    assert_eq!(
        dispatched,
        [
            (
                tid("thread:target"),
                CommandId::new("effect:effect:send").unwrap(),
                continuation(false)
            ),
            (
                tid("thread:target"),
                CommandId::new("effect:effect:send").unwrap(),
                Command::Rename {
                    title: "Child".into()
                }
            ),
        ]
    );
}

#[tokio::test]
async fn settles_a_delegated_child_once_its_restart_continuation_fails_for_good() {
    let threads = Arc::new(Threads::default());
    *threads.setting.lock().unwrap() =
        Some(Err(RuntimeError::InvalidInput("provider instance removed")));
    let handler = SendToThread::new(threads.clone());

    assert!(handler.run(job(continuation(true), true)).await.is_err());
    assert!(handler.run(job(continuation(true), false)).await.is_err());
    assert!(threads.dispatched.lock().unwrap().is_empty());
    // The worker feeds the failure to the sending thread with the row's settlement.
    let failed = job(continuation(true), false);
    assert_eq!(
        handler.failure(&failed.effect, "provider instance removed"),
        Some(EffectResult::ThreadCommandFailed {
            thread: tid("thread:target"),
            command: Box::new(continuation(true)),
            reason: "provider instance removed".into(),
        })
    );
    assert_eq!(
        handler.failure(
            &job(
                Command::Rename {
                    title: "Child".into()
                },
                false
            )
            .effect,
            "gone"
        ),
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continuation_follows_the_setting_when_it_runs() {
    for enabled in [false, true] {
        let rig = rig();
        rig.ops.continue_enabled.store(enabled, Ordering::SeqCst);
        let id = tid("thread:continuation");
        rig.create(&id, Some(root_workspace("/repo"))).await;
        let run = rig.send(&id, "cut", "Keep going").await;
        rig.drain().await;
        let attempt = rig.attempt(&id, &run).await;
        rig.provider(
            &id,
            &attempt,
            ProviderEvent::SessionReady {
                native_thread: "native-thread".into(),
            },
        )
        .await;
        rig.provider(
            &id,
            &attempt,
            ProviderEvent::TurnStarted {
                native_turn: Some("turn-1".into()),
            },
        )
        .await;
        // The effect was recorded with `enabled: true` while the setting was on.
        rig.input(
            &id,
            Input::Recover {
                trigger: agent_domain::RecoveryTrigger::Startup,
                continue_after_restart: true,
                capturing: Default::default(),
            },
        )
        .await;
        rig.drain().await;

        let state = rig.state(&id).await;
        let continued = state.runs.iter().any(|run| run.restart_of.is_some());
        assert_eq!(continued, enabled, "enabled={enabled}");
    }
}

// A rejected child creation is not a delivery: the parent's task fails instead of
// waiting for a child that never started.
#[tokio::test(flavor = "multi_thread")]
async fn a_delegation_to_an_existing_thread_fails_the_parents_task() {
    let rig = rig();
    let parent = tid("thread:delegating-parent");
    let taken = tid("thread:already-there");
    rig.create(&parent, Some(root_workspace("/repo"))).await;
    rig.create(&taken, Some(root_workspace("/repo"))).await;
    rig.send(&parent, "parent-turn", "Delegate something").await;
    rig.drain().await;

    let reply = rig
        .command(
            &parent,
            Command::Delegate {
                task: agent_domain::NodeId::new("task:taken").unwrap(),
                child: taken.clone(),
                prompt: "Inspect the boundary".into(),
                title: None,
                selection: codex(),
                runtime_mode: agent_domain::RuntimeMode::FullAccess,
                interaction_mode: agent_domain::InteractionMode::Default,
                wake: agent_domain::CompletionWake::SettledOnly,
            },
        )
        .await;
    assert_eq!(reply, Reply::Thread(taken.clone()));
    rig.drain().await;

    let state = rig.state(&parent).await;
    assert_eq!(state.tasks[0].status, agent_domain::ItemStatus::Failed);
    assert!(
        state.tasks[0]
            .result
            .as_deref()
            .is_some_and(|result| result.contains("thread-already-exists")),
        "{:?}",
        state.tasks[0].result
    );
    assert!(
        rig.outbox_kinds(&parent)
            .await
            .contains(&("SendToThread".to_owned(), crate::EffectStatus::Failed))
    );
    assert!(rig.state(&taken).await.runs.is_empty());
}
