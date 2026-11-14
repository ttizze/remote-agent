//! A subscription reads the durable log on demand. SQLite serializes writes;
//! the stream owns only its cursor and bounded transport frames.
use super::*;
use crate::host_rpc::routing::{Credit, HostReply, HostSubscription};
use agent_protocol::{
    orchestration::projection::ThreadStreamItem,
    protocol::{self, Response},
};
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

pub struct Subscription {
    storage: Arc<Storage>,
    target: SessionRef,
    changed: tokio::sync::watch::Receiver<Option<Cursor>>,
    sequence: u64,
    bytes: Arc<AtomicUsize>,
    queued_bytes: Arc<AtomicUsize>,
    pending: VecDeque<(Vec<u8>, Credit)>,
    in_flight: Option<Credit>,
}

fn through(items: &[ThreadStreamItem], mut sequence: u64) -> u64 {
    for item in items {
        match item {
            ThreadStreamItem::Snapshot(snapshot) => sequence = snapshot.snapshot_sequence,
            ThreadStreamItem::Event { sequence: next, .. } => sequence = *next,
            ThreadStreamItem::Synchronized => {}
        }
    }
    sequence
}

pub(super) async fn subscribe(
    storage: Arc<Storage>,
    request: &records::Subscribe,
    provider: ProviderKind,
    capabilities: agent_protocol::session::Capabilities,
    project_root: Option<String>,
    bytes: Arc<AtomicUsize>,
) -> Result<HostReply, String> {
    let changed = storage.changes();
    let read = storage.clone();
    let target = request.session.clone();
    let after = request.after_sequence;
    let initial = tokio::task::spawn_blocking(move || read.thread_stream(&target, after))
        .await
        .map_err(|error| error.to_string())??;
    let sequence = through(&initial, after.unwrap_or(0));
    let frame = protocol::response_frame(Response::Success {
        result: records::Opened {
            session: request.session.clone(),
            subscription_id: request.subscription,
            initial,
            provider,
            capabilities,
            project_root,
        }
        .into(),
    })
    .map_err(|error| error.to_string())?;
    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let initial_credit = Credit::reserve(&bytes, &queued_bytes, frame.capacity())?;
    Ok(HostReply {
        initial: frame,
        updates: Some(HostSubscription::Conversation(Subscription {
            storage,
            target: request.session.clone(),
            changed,
            sequence,
            bytes,
            queued_bytes,
            pending: VecDeque::new(),
            in_flight: Some(initial_credit),
        })),
    })
}

impl Subscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.in_flight = None;
        loop {
            if let Some((frame, credit)) = self.pending.pop_front() {
                self.in_flight = Some(credit);
                return Some(frame);
            }
            self.changed.changed().await.ok()?;
            self.changed.borrow_and_update();
            let storage = self.storage.clone();
            let target = self.target.clone();
            let after = self.sequence;
            let items =
                tokio::task::spawn_blocking(move || storage.thread_stream(&target, Some(after)))
                    .await
                    .ok()?
                    .ok()?;
            self.sequence = through(&items, self.sequence);
            for item in items
                .into_iter()
                .filter(|item| !matches!(item, ThreadStreamItem::Synchronized))
            {
                let frame = protocol::encode(item).ok()?;
                let credit =
                    Credit::reserve(&self.bytes, &self.queued_bytes, frame.capacity()).ok()?;
                self.pending.push_back((frame, credit));
            }
        }
    }
}
