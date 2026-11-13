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

pub(super) fn unchanged(message: &records::Message) -> bool {
    matches!(message, records::Message::Changes { rows, removed, .. }
        if rows.is_empty() && removed.is_empty())
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
            if unchanged(&message) {
                continue;
            }
            self.cursor = through(&message);
            return protocol::encode(message).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn item_only_commits_do_not_advance_the_list_stream() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
        let id = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        storage
            .import(
                &id,
                &agent_protocol::models::ThreadResponse {
                    thread: agent_protocol::models::Thread {
                        id: Some(id.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Some(&id),
                100,
            )
            .unwrap();
        let context = Context {
            preparation: None,
            rewind_binding: None,
            imported_turns: &[],
            external_busy: false,
            now: 100,
            seed: Uuid::new_v4(),
            steers: true,
            steer_ready: true,
        };
        let attempt = Uuid::new_v4();
        storage
            .decide(
                &id,
                Event::ObservedStart {
                    execution: attempt,
                    provider: ProviderKind::Codex,
                    source: id.id.clone(),
                },
                context,
                None,
            )
            .unwrap();
        let initial = storage.list(None).unwrap();
        let cursor = through(&initial);
        let mut subscription = Subscription {
            changed: storage.changes(),
            storage: storage.clone(),
            cursor,
        };
        let committed = storage
            .decide(
                &id,
                Event::Provider {
                    attempt,
                    native_turn: None,
                    native_items: vec![],
                    observation: Observation::Item {
                        id: "reply".into(),
                        status: agent_protocol::execution::ItemStatus::Running,
                        deferred: false,
                        body: agent_protocol::items::ItemBody::AssistantText {
                            text: "partial".into(),
                            phase: Default::default(),
                        },
                    },
                },
                context,
                None,
            )
            .unwrap();
        assert!(committed.cursor.sequence > cursor.sequence);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), subscription.recv())
                .await
                .is_err()
        );
        assert_eq!(subscription.cursor, cursor);
        storage
            .decide(&id, Event::Title("renamed".into()), context, None)
            .unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), subscription.recv())
            .await
            .unwrap()
            .unwrap();
        let message = protocol::decode(&frame).unwrap();
        assert!(
            matches!(&message, records::Message::Changes { from, rows, .. } if *from == cursor && !rows.is_empty())
        );
        let replica = records::Replica::new(agent_protocol::sync::Scope::List, 1)
            .apply(1, initial)
            .unwrap()
            .apply(1, message)
            .unwrap();
        let row = &replica.rows()[&agent_protocol::sync::Key::Thread(id)];
        let agent_protocol::sync::Content::Summary(record) = &row.content else {
            panic!("thread summary")
        };
        let records::Record::Thread(header) = record.as_ref() else {
            panic!("thread header")
        };
        assert_eq!(header.state.name, "renamed");
    }
}
