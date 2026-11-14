//! Resume from the committed T3 log, falling back to a bounded snapshot.
//! The owner subscribes to notifications before this transactional read and
//! deduplicates the overlap by sequence, as does T3's thread subscription.
use super::*;
use agent_protocol::orchestration::{self as model, projection::ThreadStreamItem};

impl Storage {
    pub(in crate::host_rpc) fn thread_stream(
        &self,
        id: &SessionRef,
        after: Option<u64>,
    ) -> Result<Vec<ThreadStreamItem>, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let items = resume(&tx, &id.application_id(), after)?;
            tx.commit().map_err(failure)?;
            Ok(items)
        })
    }

    pub(in crate::host_rpc) fn thread_history(
        &self,
        id: &SessionRef,
        cursor: &str,
    ) -> Result<model::projection::ThreadHistoryPage, String> {
        self.with(|connection| {
            let tx = connection.transaction().map_err(failure)?;
            let page = timeline::history(&tx, &id.application_id(), cursor)?;
            tx.commit().map_err(failure)?;
            Ok(page)
        })
    }

    pub(in crate::host_rpc) fn thread_item(
        &self,
        id: &SessionRef,
        item: &str,
    ) -> Result<Option<model::TurnItem>, String> {
        self.with(|connection| {
            Ok(projections::read::<model::TurnItem>(connection, item)?
                .filter(|item| item.thread_id == id.application_id()))
        })
    }

    pub(in crate::host_rpc) fn thread_header(
        &self,
        id: &SessionRef,
    ) -> Result<Option<model::Thread>, String> {
        self.with(|connection| Ok(projections::read(connection, &id.application_id())?))
    }

    pub(in crate::host_rpc) fn thread_item_attempt(
        &self,
        item: &model::TurnItem,
    ) -> Result<Option<Uuid>, String> {
        self.with(|connection| {
            let Some(id) = &item.provider_turn_id else {
                return Ok(None);
            };
            let turn = projections::read::<model::ProviderTurn>(connection, id)?;
            turn.and_then(|turn| turn.run_attempt_id)
                .map(|id| Uuid::parse_str(&id).map_err(failure))
                .transpose()
        })
    }

    pub(in crate::host_rpc) fn thread_project_root(
        &self,
        id: &SessionRef,
    ) -> Result<Option<String>, String> {
        self.with(|connection|connection.query_row("SELECT p.workspace_root FROM orchestration_v2_projection_threads t JOIN projection_projects p ON p.project_id=t.project_id WHERE t.thread_id=?1",[id.application_id()],|row|row.get(0)).optional().map_err(failure))
    }
}

pub(super) fn resume(
    connection: &Connection,
    thread_id: &str,
    after: Option<u64>,
) -> Result<Vec<ThreadStreamItem>, String> {
    let thread = projections::read::<model::Thread>(connection, thread_id)?
        .ok_or("conversation does not exist")?;
    let high_water = event_store::head(connection, Some(thread_id))?;
    let replay = after
        .map(|after| {
            replay(
                connection,
                thread_id,
                after,
                high_water,
                thread.deleted_at.is_none(),
            )
        })
        .transpose()?
        .flatten();
    let mut items = match replay {
        Some(events) => events,
        None => vec![ThreadStreamItem::Snapshot(timeline::snapshot(
            connection, thread_id,
        )?)],
    };
    items.push(ThreadStreamItem::Synchronized);
    Ok(items)
}

fn replay(
    connection: &Connection,
    thread_id: &str,
    after: u64,
    through: u64,
    exists: bool,
) -> Result<Option<Vec<ThreadStreamItem>>, String> {
    let pruned:i64=connection.query_row("SELECT coalesce((SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='event-log-pruned-through'),0)",[],|row|row.get(0)).map_err(failure)?;
    if after > through || (after < through && after < pruned.try_into().map_err(failure)?) {
        return Ok(None);
    }
    // Inspect lengths before deserializing; a single historical tool body may
    // be much larger than today's projected snapshot.
    let (count,bytes,created):(i64,i64,bool)=connection.query_row("SELECT count(*),coalesce(sum(length(CAST(payload_json AS BLOB))+length(CAST(metadata_json AS BLOB))),0),coalesce(max(event_type='thread.created'),0)
        FROM (SELECT payload_json,metadata_json,event_type FROM orchestration_events
        WHERE aggregate_kind='thread' AND application_event_version=2 AND stream_id=?1 AND sequence>?2 AND sequence<=?3 ORDER BY sequence LIMIT 129)",
        params![thread_id,integer(after)?,integer(through)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).map_err(failure)?;
    if count > 128 || bytes > 1_048_576 || (created && exists) {
        return Ok(None);
    }
    let events = event_store::read(connection, Some(thread_id), after, through, 128)?;
    let mut items = Vec::with_capacity(events.len());
    let mut encoded_bytes = 0usize;
    for stored in events {
        let item = ThreadStreamItem::Event {
            sequence: stored.sequence,
            event: model::wire::event(&stored.event),
        };
        encoded_bytes =
            encoded_bytes.saturating_add(serde_json::to_vec(&item).map_err(failure)?.len());
        if encoded_bytes > 1_048_576 {
            return Ok(None);
        }
        items.push(item);
    }
    Ok(Some(items))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{NOW, database, thread};
    use super::*;

    fn rename(db: &mut Connection, thread_id: &str, title: &str) -> u64 {
        let tx = db.transaction().unwrap();
        let mut saved = projections::read::<model::Thread>(&tx, thread_id)
            .unwrap()
            .unwrap();
        saved.title = title.into();
        let event = model::Event {
            id: Uuid::new_v4().to_string(),
            thread_id: thread_id.into(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: NOW.into(),
            change: model::Change::Thread(saved),
        };
        let sequence = event_store::append(&tx, &event, None, None)
            .unwrap()
            .sequence;
        tx.commit().unwrap();
        sequence
    }

    fn fixture() -> Connection {
        let mut db = database();
        let tx = db.transaction().unwrap();
        for id in ["thread", "other"] {
            projections::save(&tx, None, &thread(id)).unwrap();
        }
        tx.commit().unwrap();
        db
    }

    #[test]
    fn reconnect_replays_committed_values_and_only_this_threads_events() {
        let mut db = fixture();
        let first = rename(&mut db, "thread", "before");
        rename(&mut db, "other", "foreign");
        assert!(
            matches!(&resume(&db,"thread",None).unwrap()[0],ThreadStreamItem::Snapshot(snapshot) if snapshot.snapshot_sequence==first)
        );
        let last = rename(&mut db, "thread", "after");
        let items = resume(&db, "thread", Some(0)).unwrap();
        assert!(
            matches!(&items[0],ThreadStreamItem::Event {sequence,event} if *sequence==first && matches!(&event.change,model::Change::Thread(thread) if thread.title=="before"))
        );
        assert!(
            matches!(&items[1],ThreadStreamItem::Event {sequence,event} if *sequence==last && event.thread_id=="thread")
        );
        assert_eq!(items.last(), Some(&ThreadStreamItem::Synchronized));
        assert_eq!(
            resume(&db, "thread", Some(last)).unwrap(),
            [ThreadStreamItem::Synchronized]
        );
        assert!(matches!(
            resume(&db, "thread", None).unwrap()[0],
            ThreadStreamItem::Snapshot(_)
        ));
    }

    #[test]
    fn unavailable_or_excessive_replay_is_replaced_by_a_snapshot() {
        let mut db = fixture();
        let first = rename(&mut db, "thread", "first");
        let last = rename(&mut db, "thread", "last");
        db.execute("INSERT INTO orchestration_v2_projection_metadata VALUES ('event-log-pruned-through',2,?1,?2)",params![integer(first).unwrap(),NOW]).unwrap();
        assert!(matches!(
            resume(&db, "thread", Some(0)).unwrap()[0],
            ThreadStreamItem::Snapshot(_)
        ));
        assert!(matches!(
            resume(&db, "thread", Some(last + 1)).unwrap()[0],
            ThreadStreamItem::Snapshot(_)
        ));
        assert!(matches!(
            resume(&db, "thread", Some(first)).unwrap()[0],
            ThreadStreamItem::Event { .. }
        ));
        assert_eq!(
            resume(&db, "thread", Some(last)).unwrap(),
            [ThreadStreamItem::Synchronized]
        );
        for index in 0..129 {
            rename(&mut db, "thread", &index.to_string());
        }
        assert!(matches!(
            resume(&db, "thread", Some(last)).unwrap()[0],
            ThreadStreamItem::Snapshot(_)
        ));
        let after = event_store::head(&db, Some("thread")).unwrap();
        rename(&mut db, "thread", &"x".repeat(1_048_577));
        assert!(matches!(
            resume(&db, "thread", Some(after)).unwrap()[0],
            ThreadStreamItem::Snapshot(_)
        ));
    }
}
