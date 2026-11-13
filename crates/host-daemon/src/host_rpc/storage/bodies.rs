//! Deferred native bodies become canonical rows only if the observed version
//! still exists. Native reads happen outside the writer and conversation owner.
use super::*;
use agent_protocol::{ids::ItemId, items::ItemBody, operations::ReadItem};

pub(in crate::host_rpc) struct NativeBody {
    pub version: u64,
    pub request: ReadItem,
}

impl Storage {
    pub(in crate::host_rpc) fn native_body(
        &self,
        target: &SessionRef,
        id: &ItemId,
    ) -> Result<Option<NativeBody>, String> {
        self.with(|connection| {
            let key = thread_key(connection, target)?;
            let Some(_) = rows::item(connection, key, id)?.filter(|item| item.deferred) else {
                return Ok(None);
            };
            let address = connection.query_row(
                "SELECT i.seq,s.provider,s.native_id,e.native_turn_id,n.native_id FROM items i JOIN item_natives n ON n.thread=i.thread AND n.item_id=i.id JOIN executions e ON e.thread=n.thread AND e.turn_id=n.turn_id AND e.attempt=n.attempt JOIN sources s ON s.id=e.source_id WHERE i.thread=?1 AND i.id=?2 AND e.native_turn_id IS NOT NULL ORDER BY e.started_seq DESC LIMIT 1",
                params![key,id.as_str()],
                |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?)),
            ).optional().map_err(failure)?.ok_or("native item address is unavailable")?;
            Ok(Some(NativeBody {
                version: address.0.try_into().map_err(failure)?,
                request: ReadItem {
                    thread_id: SessionRef::new(provider(&address.1)?, address.2).map_err(failure)?,
                    turn_id: address.3.into(),
                    item_id: address.4.into(),
                },
            }))
        })
    }

    pub(in crate::host_rpc) fn hydrate_body(
        &self,
        target: &SessionRef,
        id: &ItemId,
        version: u64,
        body: ItemBody,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let key = thread_key(&transaction, target)?;
            let mut state = rows::load(&transaction, key, self.process)?;
            let mut committed = Committed {
                state: None,
                step: Step::default(),
                cursor: cursor(&transaction)?,
                rows: vec![],
                removed: vec![],
                reset: false,
            };
            let current = transaction
                .query_row(
                    "SELECT seq FROM items WHERE thread=?1 AND id=?2",
                    params![key, id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(failure)?;
            if current == Some(integer(version)?)
                && let Some(mut item) =
                    rows::item(&transaction, key, id)?.filter(|item| item.deferred)
            {
                let turn =
                    rows::turn(&transaction, key, &item.turn)?.ok_or("item turn is missing")?;
                state.turns.insert(turn.id.clone(), turn);
                item.body = body;
                item.deferred = false;
                committed.cursor = Self::advance(&transaction)?;
                rows::write(
                    &transaction,
                    &rows::WriteScope {
                        key,
                        id: target,
                        sequence: committed.cursor.sequence,
                        at: 0,
                        process: self.process,
                    },
                    &Change::Item(item),
                    &state,
                    &mut committed.rows,
                    &mut committed.removed,
                )?;
                committed.step.outcome = Outcome::Accepted;
            }
            committed.state = Some(state);
            transaction.commit().map_err(failure)?;
            Ok(committed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        execution::{ItemStatus, TurnStatus},
        items::{AssistantPhase, Item},
        models::{Thread, ThreadResponse, Turn},
    };

    #[test]
    fn forked_details_keep_native_addresses_and_only_the_observed_version_can_be_filled() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let storage = Storage::open(&path);
        let source = SessionRef::new(ProviderKind::Codex, "external".into()).unwrap();
        let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let mut item = Item::new(
            "item".into(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "summary".into(),
                phase: AssistantPhase::Final,
            },
        );
        item.defer();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(source.clone()),
                updated_at: Some(1.0),
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(item)]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        };
        storage
            .import(&source, &response, Some(&source), 100)
            .unwrap();
        let before = storage
            .native_body(&source, &"item".into())
            .unwrap()
            .unwrap();
        storage.fork(&source, &fork, &"turn".into(), 200).unwrap();
        drop(storage);
        let storage = Storage::open(&path);
        let native = storage.native_body(&fork, &"item".into()).unwrap().unwrap();
        assert_eq!(native.request.thread_id, source);
        assert_eq!(native.request.turn_id.as_str(), "turn");
        assert_eq!(native.request.item_id.as_str(), "item");
        assert!(
            storage
                .item(&fork, &"item".into())
                .unwrap()
                .unwrap()
                .deferred
        );
        let updated = storage.header(&fork).unwrap().details.updated_at;
        let body = ItemBody::AssistantText {
            text: "full body".into(),
            phase: AssistantPhase::Final,
        };
        let stale = storage
            .hydrate_body(&fork, &"item".into(), before.version, body.clone())
            .unwrap();
        assert!(stale.rows.is_empty());
        assert!(
            storage
                .item(&fork, &"item".into())
                .unwrap()
                .unwrap()
                .deferred
        );
        let filled = storage
            .hydrate_body(&fork, &"item".into(), native.version, body.clone())
            .unwrap();
        assert_eq!(filled.rows.len(), 1);
        assert!(filled.cursor.sequence > native.version);
        let item = storage.item(&fork, &"item".into()).unwrap().unwrap();
        assert!(!item.deferred);
        assert_eq!(item.body, body);
        assert_eq!(storage.header(&fork).unwrap().details.updated_at, updated);
        let duplicate = storage
            .hydrate_body(
                &fork,
                &"item".into(),
                native.version,
                ItemBody::AssistantText {
                    text: "stale body".into(),
                    phase: AssistantPhase::Final,
                },
            )
            .unwrap();
        assert!(duplicate.rows.is_empty());
        assert_eq!(
            storage.item(&fork, &"item".into()).unwrap().unwrap().body,
            body
        );
    }
}
