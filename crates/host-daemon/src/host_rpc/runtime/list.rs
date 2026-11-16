use super::*;
use crate::host_rpc::routing::{HostReply, HostSubscription};
use agent_protocol::{
    orchestration::shell::{ShellSnapshot, ShellStreamItem, ThreadShell},
    protocol::{self, Response},
};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub struct Subscription {
    storage: Arc<Storage>,
    changed: tokio::sync::watch::Receiver<()>,
    snapshot: Arc<ShellSnapshot>,
    pending: VecDeque<ShellStreamItem>,
    sent: HashMap<String, (ThreadShell, Instant)>,
}

/// Text events update the thread clock, but do not stream its unchanged shell
/// on every character. A periodic update still advances reconnect cursors.
fn same_metadata(previous: &ThreadShell, current: &ThreadShell) -> bool {
    let mut current = current.clone();
    current
        .thread
        .updated_at
        .clone_from(&previous.thread.updated_at);
    previous == &current
}

pub(super) async fn subscribe(
    storage: Arc<Storage>,
    request: &records::SubscribeList,
    provider_errors: serde_json::Map<String, serde_json::Value>,
) -> Result<HostReply, String> {
    // Watch first: a commit between this transaction and recv must be observed.
    let changed = storage.changes();
    let read = storage.clone();
    let after = request.after_sequence;
    let (snapshot, initial) = tokio::task::spawn_blocking(move || read.open_shell(after))
        .await
        .map_err(|error| error.to_string())??;
    let now = Instant::now();
    let sent = snapshot
        .threads
        .iter()
        .chain(&snapshot.archived_threads)
        .map(|thread| (thread.thread.id.clone(), (thread.as_ref().clone(), now)))
        .collect();
    Ok(HostReply {
        initial: protocol::response_frame(Response::Success {
            result: records::OpenedList {
                subscription_id: request.subscription,
                initial,
                provider_errors,
            }
            .into(),
        })
        .map_err(|error| error.to_string())?,
        updates: Some(HostSubscription::List(Subscription {
            storage,
            changed,
            snapshot: Arc::new(snapshot),
            pending: VecDeque::new(),
            sent,
        })),
    })
}

impl Subscription {
    fn publish(&mut self, item: ShellStreamItem, now: Instant) {
        match &item {
            ShellStreamItem::ThreadUpdated { thread, .. } => {
                if self
                    .sent
                    .get(&thread.thread.id)
                    .is_some_and(|(previous, sent)| {
                        now.duration_since(*sent) < Duration::from_secs(5)
                            && same_metadata(previous, thread)
                    })
                {
                    return;
                }
                self.sent
                    .insert(thread.thread.id.clone(), (thread.as_ref().clone(), now));
            }
            ShellStreamItem::ThreadRemoved { thread_id, .. } => {
                self.sent.remove(thread_id);
            }
            ShellStreamItem::Snapshot { snapshot, .. } => {
                self.sent = snapshot
                    .threads
                    .iter()
                    .chain(&snapshot.archived_threads)
                    .map(|thread| (thread.thread.id.clone(), (thread.as_ref().clone(), now)))
                    .collect();
            }
            _ => {}
        }
        self.pending.push_back(item);
    }

    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return protocol::encode(item).ok();
            }
            self.changed.changed().await.ok()?;
            self.changed.borrow_and_update();
            let storage = self.storage.clone();
            let previous = self.snapshot.clone();
            let (snapshot, updates) =
                tokio::task::spawn_blocking(move || storage.shell_changes(&previous))
                    .await
                    .ok()?
                    .ok()?;
            self.snapshot = Arc::new(snapshot);
            let now = Instant::now();
            for item in updates {
                self.publish(item, now);
            }
        }
    }
}

#[cfg(test)]
mod tests;
