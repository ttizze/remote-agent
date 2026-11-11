//! Input sent while a turn runs waits here, as T3 Code's queued messages: the
//! Host starts each in order once the turn before it ends. A stopped or failed
//! turn holds the queue until it is resumed. The queue lives with the Host's
//! live execution, so every client shows and changes the same one.
use super::*;
use agent_protocol::{
    operations::{FollowUp, InputQueue, QueueChange},
    session::{SessionChange, SessionRef, SubmissionDelivery},
};

#[cfg(test)]
mod tests;

/// Queued input the Host delivers stays queued; only its own delivery reads this.
const STILL_QUEUED: &str = "input_still_queued";

impl HostRpcService {
    /// The receipt of queued input has no turn; the queue owns it from here.
    pub(super) fn enqueue(
        &self,
        root: &SessionRef,
        input: &op::Submission,
        mut queue: InputQueue,
    ) -> Result<op::SubmissionReceipt, Failure> {
        if queue
            .inputs
            .iter()
            .any(|queued| queued.client_user_message_id == input.client_user_message_id)
        {
            return Err(Failure::new(STILL_QUEUED, "input is still queued"));
        }
        queue.inputs.push(op::Submission {
            thread_id: root.clone(),
            ..input.clone()
        });
        self.inner
            .router
            .session_change(root, SessionChange::Queue { queue });
        // The turn may have ended while this input waited for the decision.
        if !self.inner.router.running(root) {
            self.schedule_queue(root.clone());
        }
        Ok(op::SubmissionReceipt { turn_id: None })
    }

    /// Applies a provider change. A turn that ends or answers starts the next
    /// queued input, and a failed turn holds the queue, as T3 Code does.
    pub(super) fn apply_change(
        &self,
        change: crate::providers::AgentChange,
        bindings: &[(SessionRef, ProviderKind)],
    ) -> Result<(), String> {
        use crate::providers::AgentChange;
        let ended = match &change {
            AgentChange::Session {
                session,
                change:
                    SessionChange::Turn {
                        turn,
                        completed: true,
                    },
            } => Some((
                session.clone(),
                turn.status == agent_protocol::execution::TurnStatus::Failed,
            )),
            AgentChange::Session {
                session,
                change: SessionChange::Status { .. },
            } => Some((session.clone(), false)),
            _ => None,
        };
        self.inner.router.apply(change, bindings)?;
        match ended {
            Some((session, true)) => {
                self.hold_queue(&session, true);
            }
            Some((session, false)) => self.schedule_queue(session),
            None => {}
        }
        Ok(())
    }

    /// Starts the next queued input unless something holds the queue; input
    /// the running turn cannot take yet stays queued.
    pub(super) fn schedule_queue(&self, target: SessionRef) {
        if !self.inner.router.queue_ready(&target) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(failure) = service.start_queued(&target).await {
                tracing::warn!(target: "agent", operation = "session.queue.start", message = %failure);
            }
        });
    }

    /// Queues saved before the Host restarted. The restart ended the turns
    /// they waited for, so they come back held, as T3 Code's recovery holds
    /// queued runs, and their senders see them accepted.
    pub(super) fn restore_queues(&self) {
        let queues = match self.inner.store.queues() {
            Ok(queues) => queues,
            Err(message) => {
                tracing::error!(target: "agent", operation = "session.queue.restore", message);
                return;
            }
        };
        for (target, mut queue) in queues {
            queue.held = true;
            for input in &queue.inputs {
                self.inner.router.session_change(
                    &target,
                    SessionChange::Submission {
                        id: input.client_user_message_id.clone(),
                        delivery: SubmissionDelivery::Accepted { turn_id: None },
                    },
                );
            }
            self.inner
                .router
                .session_change(&target, SessionChange::Queue { queue });
        }
    }

    /// A stop or a failed turn leaves the rest of the queue for the user.
    /// Whether waiting input changed to `held`.
    pub(super) fn hold_queue(&self, target: &SessionRef, held: bool) -> bool {
        let mut queue = self.inner.router.queue(target);
        if queue.inputs.is_empty() || queue.held == held {
            return false;
        }
        queue.held = held;
        self.inner
            .router
            .session_change(target, SessionChange::Queue { queue });
        true
    }

    async fn start_queued(&self, target: &SessionRef) -> Result<(), Failure> {
        let _serial = self.inner.router.submission_lock(target).lock_owned().await;
        let _execution = self
            .inner
            .router
            .retain_execution(target.clone())
            .map_err(|error| Failure::new("invalid_params", error))?;
        let _workspace = self.inner.worktree_access.read().await;
        if !self.inner.router.queue_ready(target) {
            return Ok(());
        }
        let next = self.inner.router.queue(target).inputs.remove(0);
        self.deliver_queued(target, &next).await.map(|_| ())
    }

    /// Steers or starts queued input. Once it leaves the queue its delivery is
    /// settled for the client that sent it; input the Host still has to queue
    /// stays where it is. A failure holds the rest of the queue.
    async fn deliver_queued(
        &self,
        target: &SessionRef,
        input: &op::Submission,
    ) -> Result<bool, Failure> {
        let id = &input.client_user_message_id;
        let running = self.inner.router.running_turn(target);
        let result = self.submit_input(target, input).await;
        if result
            .as_ref()
            .is_err_and(|error| error.code == STILL_QUEUED)
        {
            return Ok(false);
        }
        // Input that started its own turn, rather than steering the running
        // one, shows that it waited in the queue.
        if let Ok(op::SubmissionReceipt {
            turn_id: Some(turn),
        }) = &result
            && running.as_ref() != Some(turn)
        {
            self.inner.router.mark_queued_turn(target, turn.clone());
        }
        let delivery = match &result {
            Ok(receipt) => SubmissionDelivery::Accepted {
                turn_id: receipt.turn_id.clone(),
            },
            Err(error) if error.delivery == agent_transport::peer::Delivery::NotSent => {
                SubmissionDelivery::Rejected
            }
            Err(_) => SubmissionDelivery::Unknown,
        };
        let mut queue = self.inner.router.queue(target);
        queue
            .inputs
            .retain(|queued| &queued.client_user_message_id != id);
        queue.held |= result.is_err();
        self.inner
            .router
            .session_change(target, SessionChange::Queue { queue });
        self.inner.router.session_change(
            target,
            SessionChange::Submission {
                id: id.clone(),
                delivery,
            },
        );
        result.map(|_| true)
    }

    fn queued_position(&self, target: &SessionRef, id: &str) -> Option<usize> {
        self.inner
            .router
            .queue(target)
            .inputs
            .iter()
            .position(|queued| queued.client_user_message_id.as_str() == id)
    }

    pub(super) async fn change_queue(
        &self,
        target: &SessionRef,
        change: &QueueChange,
    ) -> Result<agent_protocol::models::Empty, Failure> {
        let mut queue = self.inner.router.queue(target);
        let position = |id: &str| {
            self.queued_position(target, id).ok_or_else(|| {
                Failure::new(
                    "queued_input_unavailable",
                    "このメッセージは送信済みか、キューから削除されています。",
                )
            })
        };
        match change {
            QueueChange::Edit {
                client_user_message_id,
                input,
            } => {
                if input.is_empty() {
                    return Err(Failure::new(
                        "invalid_params",
                        "queued input cannot be empty",
                    ));
                }
                let index = position(client_user_message_id)?;
                queue.inputs[index].input = input.clone();
            }
            QueueChange::Remove {
                client_user_message_id,
            } => {
                queue.inputs.remove(position(client_user_message_id)?);
                self.inner
                    .router
                    .session_change(target, SessionChange::Queue { queue });
                self.inner.router.session_change(
                    target,
                    SessionChange::Submission {
                        id: client_user_message_id.clone(),
                        delivery: SubmissionDelivery::Withdrawn,
                    },
                );
                return Ok(agent_protocol::models::Empty {});
            }
            QueueChange::Move {
                client_user_message_id,
                before,
            } => {
                let input = queue.inputs.remove(position(client_user_message_id)?);
                let index = match before {
                    Some(before) => queue
                        .inputs
                        .iter()
                        .position(|queued| &queued.client_user_message_id == before)
                        .ok_or_else(|| {
                            Failure::new(
                                "queued_input_unavailable",
                                "このメッセージは送信済みか、キューから削除されています。",
                            )
                        })?,
                    None => queue.inputs.len(),
                };
                queue.inputs.insert(index, input);
            }
            QueueChange::Steer {
                client_user_message_id,
            } => {
                let input = op::Submission {
                    follow_up: FollowUp::Steer,
                    ..queue.inputs[position(client_user_message_id)?].clone()
                };
                if !self.deliver_queued(target, &input).await? {
                    return Err(Failure::new(
                        "steer_unavailable",
                        "実行中の作業がないため、追加で指示できません。",
                    ));
                }
                return Ok(agent_protocol::models::Empty {});
            }
            QueueChange::Resume => queue.held = false,
        }
        self.inner
            .router
            .session_change(target, SessionChange::Queue { queue });
        self.schedule_queue(target.clone());
        Ok(agent_protocol::models::Empty {})
    }
}
