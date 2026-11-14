use super::*;
use agent_protocol::orchestration::Thread;

pub(super) const NOW: &str = "2026-11-14T00:00:00.000Z";

pub(super) fn thread(id: &str) -> Thread {
    serde_json::from_value(serde_json::json!({
        "id":id,"createdBy":"user","creationSource":"mobile","projectId":null,
        "title":"Example","providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"example"},
        "runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,
        "activeProviderThreadId":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":id},
        "forkedFrom":null,"createdAt":NOW,"updatedAt":NOW,"archivedAt":null,"settledOverride":null,
        "settledAt":null,"lastVisitedAt":null,"deletedAt":null
    })).unwrap()
}

pub(super) fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("orchestration.sql")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db
}
