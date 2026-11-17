//! Serializes native pairing commits and removal cleanup for the same Host's private storage.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Default)]
pub(super) struct HostStorageLocks {
    hosts: Mutex<BTreeMap<String, Weak<AsyncMutex<()>>>>,
}

impl HostStorageLocks {
    pub(super) async fn lock(&self, host: &str) -> Arc<HostStorageGuard> {
        let operation = {
            let mut hosts = self.hosts.lock().expect("Host storage locks poisoned");
            hosts.retain(|_, operation| operation.strong_count() > 0);
            if let Some(operation) = hosts.get(host).and_then(Weak::upgrade) {
                operation
            } else {
                let operation = Arc::new(AsyncMutex::new(()));
                hosts.insert(host.to_owned(), Arc::downgrade(&operation));
                operation
            }
        };
        Arc::new(HostStorageGuard {
            operation: Mutex::new(Some(operation.lock_owned().await)),
        })
    }
}

/// Keeps a Host's credential and cache storage exclusive across asynchronous native lifecycle work.
#[derive(Debug, uniffi::Object)]
pub struct HostStorageGuard {
    operation: Mutex<Option<OwnedMutexGuard<()>>>,
}

#[uniffi::export]
impl HostStorageGuard {
    /// Releases exclusivity after commit or cleanup; native callers use finally or defer.
    /// Dropping the final owner also releases it, including on errors or cancellation.
    pub fn release(&self) {
        self.operation
            .lock()
            .expect("Host storage guard poisoned")
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn pairing_waits_for_suspended_removal_before_reading_or_saving_identity() {
        let locks = HostStorageLocks::default();
        let identity = Mutex::new(Some(vec![1_u8; 32]));
        let cache = Mutex::new(vec![1_u8]);
        let (shutdown, closed) = tokio::sync::oneshot::channel();
        let mut removing = Box::pin(async {
            let _operation = locks.lock("host").await;
            closed.await.unwrap();
            identity.lock().unwrap().take();
            cache.lock().unwrap().clear();
        });
        assert!(futures_util::poll!(&mut removing).is_pending());

        let mut pairing = Box::pin(async {
            let _operation = locks.lock("host").await;
            let key = identity
                .lock()
                .unwrap()
                .get_or_insert_with(|| vec![2_u8; 32])
                .clone();
            *cache.lock().unwrap() = vec![2_u8];
            key
        });
        assert!(futures_util::poll!(&mut pairing).is_pending());
        assert_eq!(*identity.lock().unwrap(), Some(vec![1_u8; 32]));
        assert_eq!(*cache.lock().unwrap(), vec![1_u8]);

        shutdown.send(()).unwrap();
        removing.await;
        let key = pairing.await;
        assert_eq!(key, vec![2_u8; 32]);
        assert_eq!(*identity.lock().unwrap(), Some(key));
        assert_eq!(*cache.lock().unwrap(), vec![2_u8]);
    }

    #[tokio::test]
    async fn removal_waits_until_in_flight_pairing_commits() {
        let locks = HostStorageLocks::default();
        let committed = Mutex::new(false);
        let (commit, saving) = tokio::sync::oneshot::channel();
        let mut pairing = Box::pin(async {
            let _operation = locks.lock("host").await;
            saving.await.unwrap();
            *committed.lock().unwrap() = true;
        });
        assert!(futures_util::poll!(&mut pairing).is_pending());

        let mut removing = Box::pin(async {
            let _operation = locks.lock("host").await;
            assert!(*committed.lock().unwrap());
        });
        assert!(futures_util::poll!(&mut removing).is_pending());
        commit.send(()).unwrap();
        pairing.await;
        removing.await;
    }

    #[tokio::test]
    async fn another_host_does_not_wait_for_removal() {
        let locks = HostStorageLocks::default();
        let _removing = locks.lock("removed-host").await;
        let _pairing = locks
            .lock("another-host")
            .now_or_never()
            .expect("another Host's identity storage must remain accessible");
    }

    #[tokio::test]
    async fn repeated_native_release_cannot_unlock_a_successor() {
        let locks = HostStorageLocks::default();
        let previous = locks.lock("host").await;
        previous.release();
        let current = locks.lock("host").await;
        previous.release();
        let mut waiting = Box::pin(locks.lock("host"));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(current);
        let _following = waiting
            .now_or_never()
            .expect("dropping the last owner must release its storage guard");
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_strand_or_replace_a_live_guard() {
        let locks = HostStorageLocks::default();
        let current = locks.lock("host").await;
        let mut cancelled = Box::pin(locks.lock("host"));
        assert!(futures_util::poll!(&mut cancelled).is_pending());
        drop(cancelled);

        let mut waiting = Box::pin(locks.lock("host"));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(current);
        let _following = waiting
            .now_or_never()
            .expect("cancelling a waiter must not strand the next storage operation");
    }
}
