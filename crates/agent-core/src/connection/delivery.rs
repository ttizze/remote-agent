//! Sends outbox entries in per-thread order, resolves their replies and
//! completes them once the Host's change has arrived.
use super::{
    Outcome,
    owner::{Event, Owner, Waiter},
    streams::rpc_failure,
};
use crate::{
    commands::outbox::{
        Delivered, DeliveryAction, PendingCommand, Request, Resolution, delivery_action,
        retry_delay_ms, should_retry,
    },
    peer::PeerError,
    protocol::Call,
    state::{DraftAttachment, merge_restored_text},
};
use agent_domain::{Command, CommandId, ThreadId};
use agent_protocol::conversation::{Committed, ErrorCode, Launched};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl Owner {
    pub(super) fn pending(&self, thread: ThreadId, command: Command) -> PendingCommand {
        let id = self.new_command_id();
        PendingCommand::new(
            thread.clone(),
            Request::Dispatch(Box::new(crate::commands::build::dispatch(
                thread, id, command,
            ))),
            self.now(),
        )
    }

    /// Queues a request; its waiter resolves when the request completes or fails.
    pub(super) fn enqueue(
        &mut self,
        entry: PendingCommand,
        waiter: Option<Waiter>,
    ) -> Result<(), PeerError> {
        let id = entry.id.clone();
        self.state_outbox().enqueue(entry).map_err(super::invalid)?;
        if let Some(waiter) = waiter {
            self.waiters.insert(id, waiter);
        }
        self.drain();
        Ok(())
    }

    fn thread_known(&self, thread: &ThreadId, entry: &CommandId) -> bool {
        self.state.thread_row(thread).is_some()
            || self
                .state
                .thread(thread)
                .is_some_and(|sync| sync.has_data())
            || self
                .state
                .outbox
                .entries
                .iter()
                .any(|other| &other.thread == thread && other.is_launch() && &other.id != entry)
    }

    /// Sends the next request of every thread whose earlier ones are settled.
    pub(super) fn drain(&mut self) {
        if !self.connected() {
            return;
        }
        for thread in self.state.outbox.ready_threads() {
            let Some(entry) = self.state.outbox.next(&thread).cloned() else {
                continue;
            };
            let exists = self.thread_known(&thread, &entry.id);
            match delivery_action(
                entry.is_launch(),
                exists,
                self.state.shell.status,
                self.connected(),
            ) {
                DeliveryAction::Wait => {}
                DeliveryAction::Remove => {
                    self.state_outbox().remove(&entry.id);
                    if entry.is_launch() {
                        self.finish(entry);
                    } else if let Some(waiter) = self.waiters.remove(&entry.id) {
                        let _ = waiter.send(Err(super::invalid("The thread no longer exists.")));
                    }
                }
                DeliveryAction::Send => self.send(entry),
            }
        }
    }

    fn send(&mut self, entry: PendingCommand) {
        self.state_outbox().sending(&entry.id);
        let sender = self.sender.clone();
        let Ok(network) = self.network() else {
            return;
        };
        let cancel = CancellationToken::new();
        network.deliveries.insert(entry.id.clone(), cancel.clone());
        let (peer, epoch) = (network.peer.clone(), network.epoch);
        let call = match &entry.request {
            Request::Dispatch(dispatch) => Call::Dispatch(dispatch.clone()),
            Request::Launch(launch) => Call::Launch(launch.clone()),
        };
        let id = entry.id;
        network.spawn(async move {
            let mut delay = Duration::from_millis(250);
            let result = loop {
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    result = request(&peer, &call) => result,
                };
                if matches!(result, Err(PeerError::RequestTimeout { .. })) && !peer.is_closed() {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(delay) => {}
                    }
                    delay = (delay * 2).min(Duration::from_secs(5));
                    continue;
                }
                break result;
            };
            let delivered = match result {
                Ok(committed) => Delivered::Committed(committed),
                Err(error) => match rpc_failure(&error) {
                    Some(failure) => Delivered::NotSent(failure),
                    None => Delivered::Unknown(error.to_string()),
                },
            };
            let _ = sender
                .send(Event::Delivered {
                    epoch,
                    id,
                    delivered,
                })
                .await;
        });
    }

    pub(super) fn delivered(&mut self, id: CommandId, delivered: Delivered) {
        if let Some(network) = self.network.as_mut() {
            network.deliveries.remove(&id);
        }
        let retry = should_retry(&delivered);
        let not_found = matches!(&delivered, Delivered::NotSent(failure)
            if ErrorCode::of(failure) == Some(ErrorCode::ThreadNotFound));
        let thread = self.state.outbox.get(&id).map(|entry| entry.thread.clone());
        let rollback = self
            .state
            .outbox
            .get(&id)
            .and_then(|entry| match &entry.request {
                Request::Dispatch(dispatch) => match &dispatch.command {
                    Command::Rollback { checkpoint, .. } => {
                        Some((dispatch.thread_id.clone(), checkpoint.clone()))
                    }
                    _ => None,
                },
                Request::Launch(_) => None,
            });
        match self.state_outbox().resolve(&id, delivered) {
            Resolution::Waiting => {
                if let Some((thread, checkpoint)) = rollback {
                    self.state
                        .rollbacks
                        .insert(id, crate::state::PendingRollback { thread, checkpoint });
                }
                self.complete_outbox();
            }
            Resolution::Failed {
                entry,
                reason,
                rejected,
            } => {
                if not_found && let Some(thread) = &thread {
                    self.thread_deleted(thread);
                }
                let reason = if rejected {
                    self.rejection_message(&entry.thread, &reason)
                } else {
                    reason
                };
                self.fail(*entry, reason);
            }
            Resolution::Uncertain => {
                let attempts = self.state.outbox.get(&id).map_or(1, |entry| entry.attempts);
                if retry && self.connected() {
                    let sender = self.sender.clone();
                    let epoch = self.epoch;
                    if let Ok(network) = self.network() {
                        network.spawn(async move {
                            tokio::time::sleep(Duration::from_millis(retry_delay_ms(attempts)))
                                .await;
                            let _ = sender.send(Event::RetryDelivery { epoch, id }).await;
                        });
                    }
                }
            }
            Resolution::Gone => {}
        }
        self.drain();
    }

    /// Completes committed requests whose sequence the shell or thread reached.
    pub(super) fn complete_outbox(&mut self) {
        let shell = self.state.shell.sequence();
        let threads = &self.state.threads;
        let done = std::sync::Arc::make_mut(&mut self.state.outbox).complete(shell, |thread| {
            threads
                .get(thread)
                .filter(|sync| sync.has_data())
                .map(|sync| sync.cursor)
        });
        for entry in done {
            self.finish(entry);
        }
    }

    fn finish(&mut self, entry: PendingCommand) {
        let target = match &entry.request {
            Request::Launch(_) => Some(entry.thread.clone()),
            Request::Dispatch(dispatch) => match &dispatch.command {
                Command::Fork { target, .. } | Command::MergeBack { target, .. } => {
                    Some(target.clone())
                }
                _ => None,
            },
        };
        if entry.navigate
            && let Some(target) = &target
        {
            self.navigate_after(&entry, target);
        }
        if let Some(waiter) = self.waiters.remove(&entry.id) {
            let outcome = target.map_or(Outcome::Applied, |id| Outcome::StartedThread {
                id: id.to_string(),
            });
            let _ = waiter.send(Ok(outcome));
        }
    }

    /// Opens the thread a launch, fork or merge back produced while the user
    /// still looks at where it was started.
    fn navigate_after(&mut self, entry: &PendingCommand, target: &ThreadId) {
        let origin = entry
            .restore
            .as_ref()
            .map(|restore| restore.draft_key.clone());
        if entry.is_launch() {
            let key = self.state.draft_key();
            if origin.as_deref() != Some(key.as_str()) {
                return;
            }
            // Text typed while the thread was created becomes its follow-up.
            if let Some(draft) = self.state.drafts.get(&key).cloned()
                && !draft.text.is_empty()
            {
                self.state.drafts.insert(target.to_string(), draft);
                if let Some(source) = self.state.drafts.get_mut(&key) {
                    source.text.clear();
                }
            }
        } else if self.state.selected_thread.as_ref() != Some(&entry.thread) {
            return;
        }
        self.select_thread(Some(target.clone()));
    }

    /// The sentence for a refusal the Host committed, naming the thread's provider.
    fn rejection_message(&self, thread: &ThreadId, reason: &str) -> String {
        let driver = self
            .state
            .thread_state(thread)
            .and_then(|state| state.thread.as_ref())
            .map(|thread| thread.selection.driver)
            .or_else(|| {
                self.state
                    .thread_row(thread)
                    .map(|row| row.selection.driver)
            });
        match driver {
            Some(driver) => crate::view::rejection::provider_rejection_message(reason, driver),
            None => crate::view::rejection::rejection_message(reason),
        }
    }

    /// A refused request: its preview disappears and the sent content returns
    /// to the composer.
    pub(super) fn fail(&mut self, entry: PendingCommand, reason: String) {
        if self
            .state
            .thread_order
            .as_ref()
            .is_some_and(|hold| hold.commands.contains(&entry.id))
        {
            self.state.thread_order = None;
        }
        if let Some(restore) = &entry.restore {
            let mut draft = self
                .state
                .drafts
                .get(&restore.draft_key)
                .cloned()
                .unwrap_or_else(|| match ThreadId::new(restore.draft_key.clone()) {
                    Ok(thread) => self.state.draft_for_thread(&thread),
                    Err(_) => self.state.default_draft.clone(),
                });
            draft.text = merge_restored_text(&draft.text, &restore.text);
            for attachment in &restore.attachments {
                if !draft
                    .attachments
                    .iter()
                    .any(|existing| existing.remote_id.as_deref() == Some(attachment.id.as_str()))
                {
                    draft
                        .attachments
                        .push(DraftAttachment::from_remote(attachment));
                }
            }
            self.state.drafts.insert(restore.draft_key.clone(), draft);
        }
        let background = matches!(&entry.request, Request::Dispatch(dispatch)
            if matches!(dispatch.command, Command::Visit { .. }));
        if !background {
            self.state.error = Some(crate::presentation::error::error_message(&reason));
        }
        if let Some(waiter) = self.waiters.remove(&entry.id) {
            let _ = waiter.send(Err(super::invalid(reason)));
        }
    }

    /// The user stopped a request that has not committed.
    pub(super) fn discard(&mut self, id: &CommandId) {
        if let Some(cancel) = self
            .network
            .as_mut()
            .and_then(|network| network.deliveries.remove(id))
        {
            cancel.cancel();
            if let Some(entry) = self.state.outbox.get(id).cloned()
                && entry.phase == crate::commands::outbox::Phase::InFlight
            {
                self.state_outbox()
                    .resolve(id, Delivered::Unknown("Request cancelled".into()));
            }
        }
        if let Some(entry) = self.state_outbox().discard(id) {
            self.fail(entry, "Stopped retrying.".into());
            self.state.error = None;
        }
    }
}

async fn request(peer: &super::Peer, call: &Call) -> Result<Committed, PeerError> {
    match call {
        Call::Launch(_) => peer
            .request::<Launched>(call)
            .await
            .map(|launched| launched.committed),
        _ => peer.request::<Committed>(call).await,
    }
}
