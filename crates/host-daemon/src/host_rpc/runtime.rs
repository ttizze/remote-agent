//! Resource execution for committed conversation decisions. Provider I/O never
//! runs inside a conversation receiver or a database transaction.
use super::{service::HostRpcService, storage::Storage};
use crate::providers::{AgentChange, Failure, RequestOrigin, SubmissionTarget};
use agent_protocol::{
    conversation::{Command, records},
    ids::{ClientInputId, RequestId},
    operations as op,
    session::{ProviderKind, SessionChange, SessionRef},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

mod bodies;
mod commands;
pub(in crate::host_rpc) use commands::{Origin, at};
mod events;
mod executor;
mod imports;
#[cfg(all(test, unix))]
mod ingress_tests;
pub(in crate::host_rpc) mod list;
mod native;
#[cfg(test)]
pub(in crate::host_rpc) mod native_fixture;
pub(in crate::host_rpc) mod native_metadata;
pub(in crate::host_rpc) mod output;
mod preparation;
pub(in crate::host_rpc) mod recovery;
#[cfg(test)]
mod replay;
mod resources;
mod rollback;
mod rpc;
pub(in crate::host_rpc) mod stream;
#[cfg(test)]
pub(in crate::host_rpc) mod t3_contract;
#[cfg(test)]
mod tests;
mod text;
mod titles;
mod workspaces;

pub(super) struct Runtime {
    pub storage: Arc<Storage>,
    resources: Mutex<HashMap<SessionRef, Arc<resources::Source>>>,
    preparations: Mutex<HashMap<SessionRef, Arc<preparation::Preparation>>>,
    native_access: HashMap<ProviderKind, tokio::sync::RwLock<()>>,
    requests: Mutex<HashMap<RequestId, resources::PendingRequest>>,
    outputs: Arc<output::Outputs>,
    updating_recovery: bool,
    /// A report committed by a newer update in this process supersedes old readers.
    /// The persisted updating marker also covers startup recovery and has a different lifetime.
    pub(in crate::host_rpc) update_report: AsyncMutex<bool>,
}

pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

pub(in crate::host_rpc) fn storage_failure(error: String) -> Failure {
    Failure::new("conversation_storage_failed", error)
}

impl Runtime {
    pub(super) fn new(
        storage: Arc<Storage>,
        providers: impl IntoIterator<Item = ProviderKind>,
        updating_recovery: bool,
    ) -> Self {
        Self {
            storage,
            resources: Default::default(),
            preparations: Default::default(),
            native_access: providers
                .into_iter()
                .map(|provider| (provider, Default::default()))
                .collect(),
            requests: Default::default(),
            outputs: Default::default(),
            updating_recovery,
            update_report: Default::default(),
        }
    }

    pub(super) fn cancel_preparations(&self) {
        for preparation in self.preparations.lock().unwrap().values() {
            if let Some((_, cancel)) = &*preparation.active.lock().unwrap() {
                cancel.cancel();
            }
        }
    }

    pub(super) fn start(service: &HostRpcService) {
        service.restore_setup_expiry();
        Self::start_list_updates(service);
        Self::start_effects(service);
    }

    fn start_list_updates(service: &HostRpcService) {
        let owner = Arc::downgrade(&service.inner);
        let mut changed = service.inner.conversation.storage.changes();
        tokio::spawn(async move {
            let mut snapshot = None;
            loop {
                let Some(inner) = owner.upgrade() else { break };
                let storage = inner.conversation.storage.clone();
                let previous = snapshot.take();
                let next = tokio::task::spawn_blocking(move || match previous {
                    Some(previous) => storage
                        .shell_changes(&previous)
                        .map(|(snapshot, changes)| (snapshot, !changes.is_empty())),
                    None => storage.shell_snapshot().map(|snapshot| (snapshot, true)),
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let published = match next {
                    Ok((next, published)) => {
                        snapshot = Some(next);
                        published
                    }
                    Err(error) => {
                        tracing::error!(target:"agent",operation="list.activity",%error);
                        false
                    }
                };
                if published && let Some(apns) = inner.router.apns() {
                    let activity = agent_protocol::orchestration::shell::activity(
                        snapshot
                            .as_ref()
                            .expect("published shell")
                            .threads
                            .iter()
                            .map(AsRef::as_ref),
                    );
                    let Ok(activity) = activity else { break };
                    if inner.providers.startup_errors().next().is_none() {
                        apns.activate(activity.display)
                    } else {
                        apns.update(activity.display)
                    }
                }
                drop(inner);
                if changed.changed().await.is_err() {
                    break;
                }
            }
        });
    }
}
