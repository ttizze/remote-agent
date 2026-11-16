use super::*;
use agent_protocol::orchestration::Thread;

pub(in crate::host_rpc) const NOW: &str = "2026-11-14T00:00:00.000Z";

pub(in crate::host_rpc) fn thread(id: &str) -> Thread {
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
    let mut db = Connection::open_in_memory().unwrap();
    Storage::initialize(&mut db).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db
}

impl Storage {
    /// Hold the actual shared connection to reproduce writer contention. A
    /// queued text delta must not need SQL before its coalesced publication.
    pub(in crate::host_rpc) fn hold_connection(
        &self,
        held: tokio::sync::oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        let _connection = self
            .connection
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = held.send(());
        let _ = release.recv();
    }
}

impl Storage {
    /// Seed only canonical native metadata, as a live child observation does.
    pub(in crate::host_rpc) fn seed_native_header(
        &self,
        thread: &agent_protocol::orchestration::Thread,
        native: &agent_protocol::orchestration::ProviderThread,
    ) -> Result<(), String> {
        self.with(|db| {
            let tx = db.transaction().map_err(failure)?;
            let source = native_bindings::header_source(thread, native)?;
            if native_bindings::owner(&tx, &source)?
                .is_some_and(|owner| owner.application_id() != thread.id)
            {
                return Err("native source is already owned".into());
            }
            native_threads::write_header(&tx, thread, native, &source)?;
            tx.commit().map_err(failure)
        })
    }
}
