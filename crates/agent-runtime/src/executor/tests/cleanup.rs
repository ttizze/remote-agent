//! ThreadDeletion.test.ts: "queues provider and resource cleanup and preserves an
//! earlier deletion timestamp", through the executors.
use super::*;
use agent_domain::{Attachment, AttachmentKind};

fn image(path: &str) -> Attachment {
    Attachment {
        kind: AttachmentKind::Image,
        source: None,
        id: path.into(),
        name: "screen.png".into(),
        mime_type: "image/png".into(),
        path: path.into(),
        size: 10,
    }
}

async fn thread_with_attachment(rig: &Rig, id: &ThreadId, cwd: &str) {
    rig.create(id, Some(worktree(cwd))).await;
    let mut first = message(
        &format!("{id}:message"),
        "Look at this",
        DispatchMode::StartImmediately,
    );
    first.attachments = vec![image(&format!("/attachments/{id}.png"))];
    assert!(matches!(
        rig.command(id, Command::Send(first)).await,
        Reply::Run(_)
    ));
    rig.drain().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deletion_cleans_up_terminals_and_attachments_of_the_thread() {
    let rig = rig();
    let id = tid("thread:delete");
    thread_with_attachment(&rig, &id, "/workspace/feature").await;
    rig.ops.log.lock().unwrap().clear();

    assert_eq!(rig.command(&id, Command::Delete).await, Reply::Accepted);
    rig.drain().await;

    assert_eq!(
        rig.ops.logged(),
        [
            "terminals thread:delete",
            "delete-attachments thread:delete /attachments/thread:delete.png",
        ]
    );
    let kinds = rig.outbox_kinds(&id).await;
    for kind in ["DetachSessions", "CleanupTerminals", "DeleteAttachments"] {
        assert!(
            kinds.iter().any(|(candidate, status)| candidate == kind
                && *status == crate::EffectStatus::Succeeded),
            "{kind}: {kinds:?}"
        );
    }
}

// T3 ResourceCleanupService.ts closes terminals by thread, whatever directory they share.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_threads_own_terminals_close_when_another_thread_shares_its_directory() {
    let rig = rig();
    let id = tid("thread:delete-shared");
    thread_with_attachment(&rig, &id, "/workspace/shared").await;
    rig.create(
        &tid("thread:neighbour"),
        Some(worktree("/workspace/shared")),
    )
    .await;
    rig.ops.log.lock().unwrap().clear();

    rig.command(&id, Command::Delete).await;
    rig.drain().await;

    assert_eq!(
        rig.ops.logged_with("terminals"),
        ["terminals thread:delete-shared"]
    );
    assert_eq!(rig.ops.logged_with("delete-attachments").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn archiving_detaches_and_cleans_up_terminals_but_keeps_attachments() {
    let rig = rig();
    let id = tid("thread:archive");
    thread_with_attachment(&rig, &id, "/workspace/archived").await;
    rig.ops.log.lock().unwrap().clear();

    rig.command(&id, Command::Archive { archived: true }).await;
    rig.drain().await;

    assert_eq!(rig.ops.logged(), ["terminals thread:archive"]);
}
