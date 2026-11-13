use super::*;
use crate::host_rpc::routing::{HostReply, HostSubscription};
use agent_protocol::protocol::{self, Response};

pub struct Subscription {
    storage: Arc<Storage>,
    changed: tokio::sync::watch::Receiver<Option<Cursor>>,
    cursor: Cursor,
}

fn through(message: &records::Message) -> Cursor {
    match message {
        records::Message::Snapshot { cursor, .. } => *cursor,
        records::Message::Changes { through, .. } => *through,
        _ => unreachable!("list sends snapshots or changes"),
    }
}

pub(super) async fn subscribe(
    storage: Arc<Storage>,
    request: &records::SubscribeList,
    provider_errors: serde_json::Map<String, serde_json::Value>,
) -> Result<HostReply, String> {
    // Register before the read transaction: a commit between the snapshot and
    // the first recv must remain visible, including for a slow connection.
    let changed = storage.changes();
    let read = storage.clone();
    let cached = request.cached;
    let initial = tokio::task::spawn_blocking(move || read.list(cached))
        .await
        .map_err(|error| error.to_string())??;
    let cursor = through(&initial);
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
            cursor,
        })),
    })
}

impl Subscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            self.changed.changed().await.ok()?;
            if *self.changed.borrow_and_update() == Some(self.cursor) {
                continue;
            }
            let storage = self.storage.clone();
            let cursor = self.cursor;
            let message = tokio::task::spawn_blocking(move || storage.list(Some(cursor)))
                .await
                .ok()?
                .ok()?;
            self.cursor = through(&message);
            return protocol::encode(message).ok();
        }
    }
}
