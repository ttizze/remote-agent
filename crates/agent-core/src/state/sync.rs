//! Idempotent application of snapshots, replays and history pages.
use crate::state::{Snapshot, ThreadCache};
use orchestration::*;
use std::sync::Arc;

pub fn shell(snapshot: &mut Snapshot, item: ShellStreamItem, now: &Timestamp) {
    match item {
        ShellStreamItem::Synchronized => snapshot.shell_synchronized = true,
        ShellStreamItem::Snapshot(shell) => {
            snapshot
                .observed_returns
                .retain(|id, _| shell.threads.iter().any(|s| &s.thread.id == id));
            snapshot.shell = Some(Arc::new(shell));
            snapshot.shell_synchronized = false;
        }
        ShellStreamItem::ThreadUpdated {
            sequence,
            archived,
            thread,
        } => {
            let Some(shell) = snapshot.shell.as_mut() else {
                return;
            };
            if sequence <= shell.snapshot_sequence {
                return;
            }
            if shell
                .threads
                .iter()
                .find(|s| s.thread.id == thread.thread.id)
                .is_some_and(crate::presentation::working)
                && !crate::presentation::working(&thread)
            {
                snapshot
                    .observed_returns
                    .insert(thread.thread.id.clone(), now.clone());
            }
            let shell = Arc::make_mut(shell);
            shell.snapshot_sequence = sequence;
            shell.threads.retain(|s| s.thread.id != thread.thread.id);
            shell
                .archived_threads
                .retain(|s| s.thread.id != thread.thread.id);
            if archived {
                shell.archived_threads.push(*thread)
            } else {
                shell.threads.push(*thread)
            }
        }
        ShellStreamItem::ThreadRemoved {
            sequence,
            thread_id,
        } => {
            if let Some(shell) = snapshot.shell.as_mut()
                && sequence > shell.snapshot_sequence
            {
                let shell = Arc::make_mut(shell);
                shell.snapshot_sequence = sequence;
                shell.threads.retain(|s| s.thread.id != thread_id);
                shell.archived_threads.retain(|s| s.thread.id != thread_id);
                snapshot.threads.remove(&thread_id);
                snapshot.observed_returns.remove(&thread_id);
                snapshot.drafts.remove(thread_id.as_str());
                if snapshot.selected_thread.as_ref() == Some(&thread_id) {
                    snapshot.selected_thread = None;
                    snapshot.editing_run = None;
                }
            }
        }
    }
}
pub fn thread(snapshot: &mut Snapshot, id: &ThreadId, item: ThreadStreamItem) {
    match item {
        ThreadStreamItem::Synchronized => {
            if let Some(cache) = snapshot.threads.get_mut(id) {
                cache.synchronized = true;
            }
        }
        ThreadStreamItem::Snapshot {
            snapshot_sequence,
            projection,
            history_cursor,
            has_more_history,
            latest_local_turn_ordinal,
        } => {
            if projection.thread.id != *id {
                return;
            }
            snapshot.threads.insert(
                id.clone(),
                ThreadCache {
                    projection: Arc::new(*projection),
                    sequence: snapshot_sequence,
                    history_cursor,
                    has_more_history,
                    synchronized: false,
                    latest_local_turn_ordinal,
                    accessed_at: snapshot.revision,
                },
            );
        }
        ThreadStreamItem::Event(stored) => {
            if stored.event.thread_id != *id {
                return;
            }
            if let Some(cache) = snapshot.threads.get_mut(id)
                && stored.sequence > cache.sequence
                && let Some(projection) = projector::apply(
                    Some(&cache.projection),
                    &stored.event,
                    projector::ProjectionOptions {
                        partial_timeline: true,
                        latest_local_turn_ordinal: cache.latest_local_turn_ordinal,
                    },
                )
            {
                cache.projection = Arc::new(projection);
                cache.sequence = stored.sequence;
            }
        }
    }
    evict(snapshot);
}
pub fn history(cache: &ThreadCache, page: ThreadHistoryPage) -> ThreadCache {
    let mut next = cache.clone();
    let projection = Arc::make_mut(&mut next.projection);
    // A live update already in the cache wins over an older history result.
    for row in page.items {
        if !projection.visible_turn_items.iter().any(|old| {
            old.source_thread_id == row.source_thread_id && old.source_item_id == row.source_item_id
        }) && !projection
            .turn_items
            .iter()
            .any(|old| old.id == row.item.id)
        {
            if row.visibility == Visibility::Local {
                projection.turn_items.push(row.item);
            } else {
                projection.visible_turn_items.push(row);
            }
        }
    }
    projection.visible_turn_items.sort_by_key(|r| r.position);
    next.history_cursor = page.next_cursor;
    next.has_more_history = page.has_more_history;
    next
}
fn evict(snapshot: &mut Snapshot) {
    while snapshot.threads.len() > 16 {
        let oldest = snapshot
            .threads
            .iter()
            .filter(|(id, _)| snapshot.selected_thread.as_ref() != Some(*id))
            .min_by_key(|(_, c)| c.accessed_at)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            snapshot.threads.remove(&id);
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use proptest::prelude::*;
    fn initial() -> Snapshot {
        let p = projection();
        let id = p.thread.id.clone();
        let mut snapshot = Snapshot::default();
        thread(
            &mut snapshot,
            &id,
            ThreadStreamItem::Snapshot {
                snapshot_sequence: 10,
                projection: Box::new(p),
                history_cursor: None,
                has_more_history: false,
                latest_local_turn_ordinal: None,
            },
        );
        snapshot
    }
    #[test]
    fn cursor_replay_does_not_regress_or_duplicate_records() {
        let mut snapshot = initial();
        let id = ThreadId::new("thread").unwrap();
        let message = item(
            "assistant",
            1,
            TurnItemBody::AssistantMessage {
                message_id: MessageId::new("message").unwrap(),
                text: "First".into(),
                attachments: vec![],
                streaming: false,
            },
        );
        let update = ThreadStreamItem::Event(Box::new(StoredEvent {
            sequence: 12,
            command_id: None,
            event: event("update", EventPayload::TurnItemUpdated(message)),
        }));
        thread(&mut snapshot, &id, update.clone());
        thread(&mut snapshot, &id, update);
        assert_eq!(snapshot.threads[&id].sequence, 12);
        assert_eq!(snapshot.threads[&id].projection.turn_items.len(), 1);
        thread(&mut snapshot, &id, ThreadStreamItem::Synchronized);
        assert!(snapshot.threads[&id].synchronized);
    }
    #[test]
    fn history_does_not_replace_newer_streamed_items() {
        let snapshot = initial();
        let id = ThreadId::new("thread").unwrap();
        let mut cache = snapshot.threads[&id].clone();
        let fresh = item(
            "answer",
            2,
            TurnItemBody::AssistantMessage {
                message_id: MessageId::new("m").unwrap(),
                text: "Fresh".into(),
                attachments: vec![],
                streaming: false,
            },
        );
        Arc::make_mut(&mut cache.projection)
            .turn_items
            .push(fresh.clone());
        let mut stale = fresh;
        stale.body = TurnItemBody::SystemNotice {
            message: "Stale".into(),
        };
        let page = ThreadHistoryPage {
            snapshot_sequence: 8,
            items: vec![ProjectedTurnItem {
                position: 1,
                visibility: Visibility::Local,
                source_thread_id: id.clone(),
                source_item_id: stale.id.clone(),
                item: stale,
            }],
            next_cursor: None,
            has_more_history: false,
        };
        let next = history(&cache, page);
        assert_eq!(
            crate::presentation::timeline(&next.projection)[0].text,
            "Fresh"
        );
        assert_eq!(next.sequence, 10);
    }
    proptest! {
        #[test]
        fn arbitrary_shell_replays_preserve_the_highest_cursor(sequences in prop::collection::vec(1u64..1000,1..100)){
            let shell=projector::shell(&projection());let mut snapshot=Snapshot{shell:Some(Arc::new(ShellSnapshot{schema_version:2,snapshot_sequence:0,threads:vec![],archived_threads:vec![]})),..Snapshot::default()};
            for sequence in &sequences{super::shell(&mut snapshot,ShellStreamItem::ThreadUpdated{sequence:*sequence,archived:false,thread:Box::new(shell.clone())},&now());}
            let shell=snapshot.shell.unwrap();prop_assert_eq!(shell.snapshot_sequence,*sequences.iter().max().unwrap());prop_assert_eq!(shell.threads.len(),1);
        }
    }
}
