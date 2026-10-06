//! Commands this device sent and the Host has not confirmed. Each thread
//! delivers in order; a retry reuses the command id. An entry completes once
//! the stream it changes reaches the committed sequence.
use super::lifecycle::LifecycleOverlay;
use crate::sync::ShellStatus;
use agent_domain::{
    Attachment, Command, CommandId, MessageContext, MessageId, Reply, State, ThreadId, Timestamp,
};
use agent_protocol::conversation::{Committed, Dispatch, Launch, ShellSnapshot};
use agent_protocol::error::{Delivery, RpcFailure};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

pub const MAX_PENDING: usize = 2048;
const MAX_RETRY_DELAY_MS: u64 = 16_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Dispatch(Box<Dispatch>),
    Launch(Box<Launch>),
}
impl Request {
    pub fn command_id(&self) -> &CommandId {
        match self {
            Self::Dispatch(dispatch) => &dispatch.command_id,
            Self::Launch(launch) => &launch.command_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    Queued,
    InFlight,
    /// The request may or may not have committed; it is sent again with the
    /// same id.
    Uncertain {
        error: String,
    },
    /// Committed; waits until the stream it changes reaches `sequence`.
    Committed {
        sequence: u64,
        reply: Reply,
    },
}

/// The composer content a send cleared, put back if the Host refuses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Restore {
    pub draft_key: String,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingCommand {
    pub id: CommandId,
    pub thread: ThreadId,
    pub request: Request,
    pub phase: Phase,
    pub created_at: Timestamp,
    pub attempts: u32,
    pub overlay: Option<LifecycleOverlay>,
    pub restore: Option<Restore>,
    /// A launch from the new-thread composer opens its thread on completion.
    pub navigate: bool,
}

impl PendingCommand {
    pub fn new(thread: ThreadId, request: Request, created_at: Timestamp) -> Self {
        Self {
            id: request.command_id().clone(),
            thread,
            request,
            phase: Phase::Queued,
            created_at,
            attempts: 0,
            overlay: None,
            restore: None,
            navigate: false,
        }
    }
    pub fn is_launch(&self) -> bool {
        matches!(self.request, Request::Launch(_))
    }
    pub fn committed_sequence(&self) -> Option<u64> {
        match &self.phase {
            Phase::Committed { sequence, .. } => Some(*sequence),
            _ => None,
        }
    }
    /// The message this entry carries, shown until the thread folds it.
    pub fn message(&self) -> Option<PendingMessage> {
        let (id, text, attachments, context) = match &self.request {
            Request::Dispatch(dispatch) => match &dispatch.command {
                Command::Send(send) => (
                    send.id.clone(),
                    send.text.clone(),
                    send.attachments.clone(),
                    send.context.clone(),
                ),
                _ => return None,
            },
            Request::Launch(launch) => {
                let message = launch.message.as_ref()?;
                (
                    message.id.clone()?,
                    message.text.clone(),
                    message.attachments.clone(),
                    message.context.clone(),
                )
            }
        };
        Some(PendingMessage {
            command: self.id.clone(),
            thread: self.thread.clone(),
            id,
            text,
            attachments,
            context,
            created_at: self.created_at.clone(),
            phase: self.phase.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingMessage {
    pub command: CommandId,
    pub thread: ThreadId,
    pub id: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
    pub created_at: Timestamp,
    pub phase: Phase,
}

/// How a delivery attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivered {
    Committed(Committed),
    /// The Host refused the request before any commit.
    NotSent(RpcFailure),
    /// The connection failed or the reply was lost; it may have committed.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Committed; completes when its sequence arrives.
    Waiting,
    Failed {
        entry: Box<PendingCommand>,
        reason: String,
    },
    Uncertain,
    /// No pending entry has this id any more.
    Gone,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outbox {
    pub entries: Vec<PendingCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryAction {
    Wait,
    Remove,
    Send,
}

/// A creation waits for a live shell, so a launch that already committed is
/// seen before it could be sent twice; a message to a thread the live shell no
/// longer lists is dropped.
pub fn delivery_action(
    is_creation: bool,
    thread_exists: bool,
    shell: ShellStatus,
    connected: bool,
) -> DeliveryAction {
    if is_creation {
        if thread_exists {
            return DeliveryAction::Remove;
        }
        return if connected && shell == ShellStatus::Live {
            DeliveryAction::Send
        } else {
            DeliveryAction::Wait
        };
    }
    if !thread_exists {
        return if shell == ShellStatus::Live {
            DeliveryAction::Remove
        } else {
            DeliveryAction::Wait
        };
    }
    if connected {
        DeliveryAction::Send
    } else {
        DeliveryAction::Wait
    }
}

/// 1 s doubling to 16 s between attempts of a request that did not reach the Host.
pub fn retry_delay_ms(attempt: u32) -> u64 {
    1_000u64
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(16))
        .min(MAX_RETRY_DELAY_MS)
}

/// Only a refusal the Host decided means the request itself is bad.
pub fn should_retry(delivered: &Delivered) -> bool {
    match delivered {
        Delivered::NotSent(failure) => failure.delivery != Delivery::NotSent,
        Delivered::Unknown(_) => true,
        Delivered::Committed(_) => false,
    }
}

impl Outbox {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn get(&self, id: &CommandId) -> Option<&PendingCommand> {
        self.entries.iter().find(|entry| &entry.id == id)
    }
    pub fn get_mut(&mut self, id: &CommandId) -> Option<&mut PendingCommand> {
        self.entries.iter_mut().find(|entry| &entry.id == id)
    }

    /// Appends a command, or replaces the pending entry a retry with the same
    /// id left behind.
    pub fn enqueue(&mut self, entry: PendingCommand) -> Result<(), String> {
        if let Some(existing) = self.get_mut(&entry.id) {
            *existing = entry;
            return Ok(());
        }
        if self.entries.len() >= MAX_PENDING {
            return Err("Too many pending commands; wait for the Host".into());
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Whether an earlier pending launch creates this thread.
    pub fn creates(&self, thread: &ThreadId) -> bool {
        self.entries
            .iter()
            .any(|entry| &entry.thread == thread && entry.is_launch())
    }

    /// The next request of a thread: its oldest entry not yet committed, unless
    /// an earlier one is in flight or waiting to be retried.
    pub fn next(&self, thread: &ThreadId) -> Option<&PendingCommand> {
        let head = self
            .entries
            .iter()
            .filter(|entry| &entry.thread == thread)
            .find(|entry| !matches!(entry.phase, Phase::Committed { .. }))?;
        (head.phase == Phase::Queued).then_some(head)
    }

    /// Threads with a request ready to send, in first-pending order.
    pub fn ready_threads(&self) -> Vec<ThreadId> {
        let mut threads: Vec<ThreadId> = vec![];
        for entry in &self.entries {
            if !threads.contains(&entry.thread) && self.next(&entry.thread).is_some() {
                threads.push(entry.thread.clone());
            }
        }
        threads
    }

    pub fn sending(&mut self, id: &CommandId) {
        if let Some(entry) = self.get_mut(id) {
            entry.phase = Phase::InFlight;
            entry.attempts += 1;
        }
    }

    /// Requests whose outcome the last connection did not report are sent
    /// again with the same ids.
    pub fn reconnected(&mut self) {
        for entry in &mut self.entries {
            if matches!(entry.phase, Phase::InFlight | Phase::Uncertain { .. }) {
                entry.phase = Phase::Queued;
            }
        }
    }

    pub fn retry(&mut self, id: &CommandId) {
        if let Some(entry) = self.get_mut(id)
            && matches!(entry.phase, Phase::Uncertain { .. })
        {
            entry.phase = Phase::Queued;
        }
    }

    pub fn resolve(&mut self, id: &CommandId, delivered: Delivered) -> Resolution {
        let Some(index) = self.entries.iter().position(|entry| &entry.id == id) else {
            return Resolution::Gone;
        };
        let failed = |entries: &mut Vec<PendingCommand>, reason: String| Resolution::Failed {
            entry: Box::new(entries.remove(index)),
            reason,
        };
        match delivered {
            Delivered::Committed(Committed {
                reply: Reply::Rejected { reason },
                ..
            }) => failed(&mut self.entries, reason),
            Delivered::Committed(committed) => {
                self.entries[index].phase = Phase::Committed {
                    sequence: committed.sequence,
                    reply: committed.reply,
                };
                Resolution::Waiting
            }
            Delivered::NotSent(failure) if failure.delivery == Delivery::NotSent => {
                failed(&mut self.entries, failure.message)
            }
            Delivered::NotSent(failure) => {
                self.entries[index].phase = Phase::Uncertain {
                    error: failure.message,
                };
                Resolution::Uncertain
            }
            Delivered::Unknown(error) => {
                self.entries[index].phase = Phase::Uncertain { error };
                Resolution::Uncertain
            }
        }
    }

    /// Removes committed entries whose change has arrived. A lifecycle preview
    /// waits for the shell so the Host's row replaces it without a flash.
    pub fn complete(
        &mut self,
        shell: Option<u64>,
        thread: impl Fn(&ThreadId) -> Option<u64>,
    ) -> Vec<PendingCommand> {
        let reached = |entry: &PendingCommand| {
            let Some(sequence) = entry.committed_sequence() else {
                return false;
            };
            shell.is_some_and(|cursor| cursor >= sequence)
                || (entry.overlay.is_none()
                    && thread(&entry.thread).is_some_and(|cursor| cursor >= sequence))
        };
        let (done, pending) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| reached(entry));
        self.entries = pending;
        done
    }

    /// The user stopped retrying a request that is not in flight.
    pub fn discard(&mut self, id: &CommandId) -> Option<PendingCommand> {
        let index = self.entries.iter().position(|entry| {
            &entry.id == id && matches!(entry.phase, Phase::Queued | Phase::Uncertain { .. })
        })?;
        Some(self.entries.remove(index))
    }

    pub fn remove(&mut self, id: &CommandId) -> Option<PendingCommand> {
        let index = self.entries.iter().position(|entry| &entry.id == id)?;
        Some(self.entries.remove(index))
    }

    /// The shell with every pending preview applied in send order. A preview
    /// the shell already reflects, or for a thread it no longer lists, is skipped.
    pub fn overlay_shell<'a>(&self, shell: &'a ShellSnapshot) -> Cow<'a, ShellSnapshot> {
        let mut result = Cow::Borrowed(shell);
        for entry in &self.entries {
            let Some(overlay) = &entry.overlay else {
                continue;
            };
            let sequence = entry.committed_sequence();
            if sequence.is_some_and(|sequence| sequence <= shell.snapshot_sequence) {
                continue;
            }
            let Some(index) = result.threads.iter().position(|row| row.id == entry.thread) else {
                continue;
            };
            let mut row = result.threads[index].clone();
            overlay.apply(&mut row, &entry.created_at, sequence.is_some());
            if row != result.threads[index] {
                result.to_mut().threads[index] = row;
            }
        }
        result
    }

    /// Messages of a thread the device sent but the thread has not folded, in
    /// send order.
    pub fn undelivered_messages(
        &self,
        thread: &ThreadId,
        state: Option<&State>,
    ) -> Vec<PendingMessage> {
        self.entries
            .iter()
            .filter(|entry| &entry.thread == thread)
            .filter_map(PendingCommand::message)
            .filter(|message| state.is_none_or(|state| state.message(&message.id).is_none()))
            .collect()
    }

    /// Launches not yet confirmed, for pending thread rows.
    pub fn pending_launches(&self) -> impl Iterator<Item = &PendingCommand> {
        self.entries.iter().filter(|entry| entry.is_launch())
    }

    /// The form stored with the device state: in-flight requests are sent again.
    pub fn persisted(&self) -> Self {
        let mut stored = self.clone();
        for entry in &mut stored.entries {
            if entry.phase == Phase::InFlight {
                entry.phase = Phase::Queued;
            }
        }
        stored
    }
}

#[cfg(test)]
mod tests;
