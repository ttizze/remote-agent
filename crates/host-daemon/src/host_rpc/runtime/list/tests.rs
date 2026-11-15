use super::*;
use agent_protocol::orchestration as model;
use serde_json::json;
const NOW: &str = "2026-11-14T00:00:00Z";

fn storage(directory: &std::path::Path) -> Arc<Storage> {
    let storage = Arc::new(Storage::open(&directory.join("state.sqlite")));
    let thread: model::Thread = serde_json::from_value(json!({
        "id":"thread","createdBy":"user","creationSource":"mobile","projectId":null,"title":"Title",
        "providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"example"},
        "runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,
        "activeProviderThreadId":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":"thread"},
        "forkedFrom":null,"createdAt":NOW,"updatedAt":NOW,"archivedAt":null,"settledOverride":null,
        "settledAt":null,"lastVisitedAt":null,"deletedAt":null
    })).unwrap();
    storage
        .thread_command(
            crate::host_rpc::storage::commands::Command {
                id: "create",
                thread_id: "thread",
                kind: "thread.create",
                now: NOW,
                item_ids: &[],
            },
            Some(&thread),
            |_| Ok(model::Decision::default()),
        )
        .unwrap();
    storage
}
fn item(text: &str) -> model::TurnItem {
    serde_json::from_value(json!({"id":"reply","threadId":"thread","runId":null,"nodeId":null,
        "providerThreadId":null,"providerTurnId":null,"nativeItemRef":null,"parentItemId":null,
        "ordinal":0,"status":"running","title":null,"startedAt":NOW,"completedAt":null,"updatedAt":NOW,
        "type":"assistant_message","messageId":"reply","text":text,"attachments":[],"streaming":true})).unwrap()
}
async fn subscription(storage: Arc<Storage>) -> Subscription {
    let reply = subscribe(
        storage,
        &records::SubscribeList {
            refresh: false,
            subscription: Uuid::new_v4(),
            after_sequence: None,
        },
        Default::default(),
    )
    .await
    .unwrap();
    let HostSubscription::List(subscription) = reply.updates.unwrap() else {
        panic!("shell subscription")
    };
    subscription
}

#[tokio::test]
async fn item_only_commits_advance_the_reader_without_sending_list_frames() {
    let directory = tempfile::tempdir().unwrap();
    let storage = storage(directory.path());
    storage
        .observe_thread("thread", &[], NOW, |_| {
            Ok(model::Decision {
                changes: vec![model::Change::TurnItem(item("first"))],
                ..Default::default()
            })
        })
        .unwrap();
    let mut subscription = subscription(storage.clone()).await;
    for round in 0..2 {
        for index in 0..1101 {
            let at = format!("2026-11-14T00:00:01.{index:06}Z");
            storage
                .observe_thread("thread", &["reply".into()], &at, |_| {
                    Ok(model::Decision {
                        changes: vec![model::Change::TurnItem(item(&format!(
                            "partial {round}-{index}"
                        )))],
                        ..Default::default()
                    })
                })
                .unwrap();
        }
        // SQL write latency is independent of the separately tested cadence.
        // Anchor the send clock here to check a recent, unchanged publication.
        subscription.sent.get_mut("thread").unwrap().1 = Instant::now();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), subscription.recv())
                .await
                .is_err()
        );
        assert_eq!(
            subscription.snapshot.snapshot_sequence,
            storage.shell_snapshot().unwrap().snapshot_sequence
        );
    }
    storage
        .observe_thread("thread", &[], NOW, |state| {
            let mut thread = state.thread.clone();
            thread.title = "renamed".into();
            Ok(model::Decision {
                changes: vec![model::Change::Thread(thread)],
                ..Default::default()
            })
        })
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .unwrap()
        .unwrap();
    let ShellStreamItem::ThreadUpdated { thread, .. } = protocol::decode(&frame).unwrap() else {
        panic!("thread shell")
    };
    assert_eq!(thread.thread.title, "renamed");
}

#[tokio::test]
async fn unchanged_metadata_is_resent_after_five_seconds_and_real_changes_are_immediate() {
    let directory = tempfile::tempdir().unwrap();
    let storage = storage(directory.path());
    let mut subscription = subscription(storage).await;
    let now = subscription.sent["thread"].1;
    let mut thread = subscription.snapshot.threads[0].as_ref().clone();
    thread.thread.updated_at = "2026-11-14T00:00:02Z".into();
    let item = ShellStreamItem::ThreadUpdated {
        sequence: 2,
        location: agent_protocol::orchestration::shell::ShellLocation::Active,
        thread: thread.clone(),
    };
    subscription.publish(item.clone(), now + Duration::from_millis(4999));
    assert!(subscription.pending.is_empty());
    subscription.publish(item, now + Duration::from_millis(5000));
    assert_eq!(subscription.pending.len(), 1);
    subscription.pending.clear();
    thread.thread.title = "updated".into();
    subscription.publish(
        ShellStreamItem::ThreadUpdated {
            sequence: 3,
            location: agent_protocol::orchestration::shell::ShellLocation::Active,
            thread,
        },
        now + Duration::from_millis(5001),
    );
    assert_eq!(subscription.pending.len(), 1);
}
