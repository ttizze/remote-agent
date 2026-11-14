use super::super::conversations::tests::{NativeAgent, call};
use super::*;
use crate::providers::AgentChange;
use agent_protocol::{
    execution::TurnStatus,
    models::{Empty, Turn},
    session::OpenedSession,
};
use std::time::Duration;

struct Running {
    directory: tempfile::TempDir,
    host: HostRpcService,
    agent: Arc<NativeAgent>,
    root: SessionRef,
    client: crate::host_rpc::routing::HostSession,
}

/// A conversation whose turn `live` runs until the test ends it.
async fn running() -> Running {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let root = agent
        .create(directory.path().to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    let host = HostRpcService::new(
        [agent.backend()],
        crate::ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    host.inner
        .router
        .session_change(&root, live(TurnStatus::Running));
    Running {
        directory,
        host,
        agent,
        root,
        client,
    }
}

fn live(status: TurnStatus) -> SessionChange {
    SessionChange::Turn {
        turn: Turn {
            id: "live".into(),
            status,
            items: Some(Vec::new()),
            ..Default::default()
        },
        completed: status != TurnStatus::Running,
    }
}

impl Running {
    /// The Host restarts over the same state; the provider's process ended.
    fn restarted(self) -> Self {
        let Self {
            directory, agent, ..
        } = self;
        let agent = agent.restarted();
        let host = HostRpcService::new(
            [agent.backend()],
            crate::ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let root = Running::session(&agent);
        Self {
            directory,
            host,
            agent,
            root,
            client,
        }
    }
    fn session(agent: &NativeAgent) -> SessionRef {
        agent
            .records
            .lock()
            .unwrap()
            .values()
            .next()
            .and_then(|record| record.thread.id.clone())
            .unwrap()
    }
    async fn send(&self, id: &str, follow_up: FollowUp) -> op::SubmissionReceipt {
        call(
            &self.host,
            self.client.id(),
            Call::Submit(op::Submission {
                thread_id: self.root.clone(),
                client_user_message_id: id.into(),
                input: vec![op::Input::Text { text: id.into() }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up,
            }),
        )
        .await
        .unwrap()
    }
    async fn change(
        &self,
        change: QueueChange,
    ) -> Result<Empty, agent_protocol::error::RpcFailure> {
        call(
            &self.host,
            self.client.id(),
            Call::ChangeQueue(op::ChangeQueue {
                thread_id: self.root.clone(),
                change,
            }),
        )
        .await
    }
    async fn thread(&self) -> Thread {
        call::<OpenedSession>(
            &self.host,
            self.client.id(),
            Call::OpenSession(agent_protocol::session::OpenSession {
                session: self.root.clone(),
                limit: 100,
                include_activity: false,
                known_version: None,
            }),
        )
        .await
        .unwrap()
        .response
        .thread
    }
    async fn queued(&self) -> Vec<String> {
        self.thread()
            .await
            .queue
            .inputs
            .into_iter()
            .map(|input| input.client_user_message_id.to_string())
            .collect()
    }
    /// The provider ends the running turn.
    fn finish(&self, status: TurnStatus) {
        self.host
            .apply_change(
                AgentChange::Session {
                    session: self.root.clone(),
                    change: live(status),
                },
                &[],
            )
            .unwrap();
    }
    fn sent(&self) -> Vec<String> {
        self.agent
            .inputs
            .lock()
            .unwrap()
            .iter()
            .map(|input| {
                let op::Input::Text { text } = &input.input[0] else {
                    panic!("fixture input is text")
                };
                format!("{}={text}", input.client_user_message_id)
            })
            .collect()
    }
    async fn until_sent(&self, expected: &[&str]) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.sent() != expected {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("sent {:?}, expected {expected:?}", self.sent()));
    }
}

#[tokio::test]
async fn incomplete_context_is_rejected_before_creating_submitting_or_editing_a_task() {
    use agent_protocol::context::{ContextContent, MessageContext};
    let running = running().await;
    running.send("kept", FollowUp::Queue).await;
    let before = running.thread().await.queue;
    let input = vec![op::Input::Context {
        context: Box::new(MessageContext {
            id: "context".into(),
            label: "Terminal".into(),
            content: ContextContent::Summary {
                kind: "terminal".into(),
            },
        }),
    }];
    let requests = [
        Call::Submit(op::Submission {
            thread_id: running.root.clone(),
            client_user_message_id: "invalid".into(),
            input: input.clone(),
            model: None,
            effort: None,
            service_tier: None,
            follow_up: FollowUp::Queue,
        }),
        Call::ChangeQueue(op::ChangeQueue {
            thread_id: running.root.clone(),
            change: QueueChange::Edit {
                client_user_message_id: "kept".into(),
                input: input.clone(),
            },
        }),
        Call::CreateSession(op::CreateSession {
            provider: ProviderKind::Codex,
            cwd: Some(running.directory.path().to_string_lossy().into_owned()),
            model: None,
            initial_input: Some(op::InitialInput {
                workspace: None,
                client_user_message_id: "new-invalid".into(),
                input,
                effort: None,
                service_tier: None,
            }),
        }),
    ];
    for request in requests {
        let error = call::<serde_json::Value>(&running.host, running.client.id(), request)
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_params");
    }
    assert_eq!(running.thread().await.queue, before);
    assert!(running.sent().is_empty());
    assert_eq!(running.agent.records.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn queued_input_waits_for_the_running_turn_and_then_runs_in_order() {
    let running = running().await;
    for id in ["first", "second"] {
        assert_eq!(running.send(id, FollowUp::Queue).await.turn_id, None);
    }
    assert!(running.sent().is_empty());
    assert_eq!(running.queued().await, ["first", "second"]);
    let thread = running.thread().await;
    assert_eq!(
        thread.submissions["first"],
        SubmissionDelivery::Accepted { turn_id: None }
    );
    running.finish(TurnStatus::Completed);
    // Each started input's turn ends before the next one starts.
    running.until_sent(&["first=first", "second=second"]).await;
    // Their delivery evidence retires once their finished turns echo them,
    // and their turns show that they waited, as T3 Code labels them.
    let thread = running.thread().await;
    assert_eq!(thread.queue, InputQueue::default());
    assert!(thread.submissions.is_empty());
    let queued: Vec<_> = thread
        .turns
        .iter()
        .flatten()
        .filter(|turn| turn.queued)
        .flat_map(|turn| turn.items.iter().flatten())
        .filter_map(|item| item.client_input_id.as_ref().map(|id| id.to_string()))
        .collect();
    assert_eq!(queued, ["first", "second"]);
}

#[tokio::test]
async fn a_steer_joins_the_running_turn_without_waiting_for_the_queue() {
    let running = running().await;
    running.send("waiting", FollowUp::Queue).await;
    running.send("steer", FollowUp::Steer).await;
    assert_eq!(running.sent(), ["steer=steer"]);
    assert_eq!(running.queued().await, ["waiting"]);
    // Waiting input moves before other waiting input, or to the end.
    running.send("later", FollowUp::Queue).await;
    for (before, expected) in [
        (Some("waiting"), ["later", "waiting"]),
        (None, ["waiting", "later"]),
    ] {
        running
            .change(QueueChange::Move {
                client_user_message_id: "later".into(),
                before: before.map(Into::into),
            })
            .await
            .unwrap();
        assert_eq!(running.queued().await, expected);
    }
    running
        .change(QueueChange::Remove {
            client_user_message_id: "later".into(),
        })
        .await
        .unwrap();
    // Queued input can steer later, which takes it out of the queue.
    running
        .change(QueueChange::Steer {
            client_user_message_id: "waiting".into(),
        })
        .await
        .unwrap();
    assert_eq!(running.sent(), ["steer=steer", "waiting=waiting"]);
    assert!(running.queued().await.is_empty());
    let missing = running
        .change(QueueChange::Steer {
            client_user_message_id: "waiting".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, "queued_input_unavailable");
}

#[tokio::test]
async fn edits_and_removals_change_waiting_input_and_a_stop_holds_the_rest() {
    let running = running().await;
    for id in ["edited", "removed", "kept"] {
        running.send(id, FollowUp::Queue).await;
    }
    running
        .change(QueueChange::Edit {
            client_user_message_id: "edited".into(),
            input: vec![op::Input::Text {
                text: "new text".into(),
            }],
        })
        .await
        .unwrap();
    running
        .change(QueueChange::Remove {
            client_user_message_id: "removed".into(),
        })
        .await
        .unwrap();
    assert_eq!(running.queued().await, ["edited", "kept"]);
    assert_eq!(
        running.thread().await.submissions["removed"],
        SubmissionDelivery::Withdrawn
    );
    call::<Empty>(
        &running.host,
        running.client.id(),
        Call::Interrupt(op::Interrupt {
            thread_id: running.root.clone(),
            turn_id: "live".into(),
        }),
    )
    .await
    .unwrap();
    running.finish(TurnStatus::Interrupted);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(running.sent().is_empty());
    let thread = running.thread().await;
    assert!(thread.queue.held);
    assert_eq!(thread.queue.inputs.len(), 2);
    // New input starts while the held queue waits for the user.
    running.send("fresh", FollowUp::Queue).await;
    running.until_sent(&["fresh=fresh"]).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(running.sent(), ["fresh=fresh"]);
    running.change(QueueChange::Resume).await.unwrap();
    running
        .until_sent(&["fresh=fresh", "edited=new text", "kept=kept"])
        .await;
}

#[tokio::test]
async fn a_failed_start_returns_its_input_and_holds_the_rest() {
    let running = running().await;
    for id in ["refused", "next"] {
        running.send(id, FollowUp::Queue).await;
    }
    running
        .agent
        .fail_submit
        .store(1, std::sync::atomic::Ordering::SeqCst);
    running.finish(TurnStatus::Completed);
    tokio::time::timeout(Duration::from_secs(10), async {
        while running.queued().await != ["next"] {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let thread = running.thread().await;
    assert!(thread.queue.held);
    assert_eq!(thread.submissions["refused"], SubmissionDelivery::Rejected);
    assert!(running.sent().is_empty());
}

#[tokio::test]
async fn a_failed_turn_holds_the_queue() {
    let running = running().await;
    running.send("waiting", FollowUp::Queue).await;
    running.finish(TurnStatus::Failed);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(running.sent().is_empty());
    assert!(running.thread().await.queue.held);
}

#[tokio::test]
async fn the_queue_survives_a_host_restart_held_until_resumed() {
    let running = running().await;
    for id in ["first", "second"] {
        running.send(id, FollowUp::Queue).await;
    }
    let running = running.restarted();
    let thread = running.thread().await;
    assert!(
        thread.queue.held,
        "the restart ended the turn they waited for"
    );
    assert_eq!(running.queued().await, ["first", "second"]);
    assert_eq!(
        thread.submissions["first"],
        SubmissionDelivery::Accepted { turn_id: None }
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(running.sent().is_empty());
    running.change(QueueChange::Resume).await.unwrap();
    running.until_sent(&["first=first", "second=second"]).await;
    // A drained queue leaves nothing to restore.
    let running = running.restarted();
    assert_eq!(running.thread().await.queue, InputQueue::default());
}
