use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

type KeyLock = (Arc<tokio::sync::Mutex<()>>, usize);

/// Serializes work for one key without coupling unrelated keys to a shared lock.
pub struct KeyedSerial<K> {
    locks: Mutex<HashMap<K, KeyLock>>,
}
impl<K: Eq + Hash + Clone> Default for KeyedSerial<K> {
    fn default() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }
}
impl<K: Eq + Hash + Clone> KeyedSerial<K> {
    pub async fn with_lock<F: Future>(&self, key: K, work: F) -> F::Output {
        let lock = {
            let mut locks = self.locks.lock().expect("keyed lock map");
            let entry = locks.entry(key.clone()).or_default();
            entry.1 += 1;
            entry.0.clone()
        };
        let _user = User { owner: self, key };
        let _permit = lock.lock().await;
        work.await
    }
    fn release(&self, key: &K) {
        let mut locks = self.locks.lock().expect("keyed lock map");
        if let Some(entry) = locks.get_mut(key) {
            entry.1 -= 1;
            if entry.1 == 0 {
                locks.remove(key);
            }
        }
    }
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.locks.lock().unwrap().len()
    }
}
struct User<'a, K: Eq + Hash + Clone> {
    owner: &'a KeyedSerial<K>,
    key: K,
}
impl<K: Eq + Hash + Clone> Drop for User<'_, K> {
    fn drop(&mut self) {
        self.owner.release(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::{Notify, mpsc};

    #[tokio::test]
    async fn allows_unrelated_keys_to_run_concurrently() {
        let executor = Arc::new(KeyedSerial::<String>::default());
        let (arrivals, mut arrived) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let runs: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|key| {
                let (executor, arrivals, release) =
                    (executor.clone(), arrivals.clone(), release.clone());
                tokio::spawn(async move {
                    let released = release.notified();
                    executor
                        .with_lock(key.to_string(), async move {
                            arrivals.send(key).unwrap();
                            released.await;
                        })
                        .await
                })
            })
            .collect();
        let observed = tokio::time::timeout(Duration::from_secs(1), async {
            let mut observed = vec![arrived.recv().await.unwrap(), arrived.recv().await.unwrap()];
            observed.sort();
            observed
        })
        .await
        .unwrap();
        release.notify_waiters();
        for run in runs {
            run.await.unwrap();
        }
        assert_eq!(observed, ["a", "b"]);
        assert_eq!(executor.tracked(), 0);
    }

    #[tokio::test]
    async fn serializes_work_for_the_same_key() {
        let executor = Arc::new(KeyedSerial::<String>::default());
        let events = Arc::new(Mutex::new(Vec::<&str>::new()));
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release_tx, release) = tokio::sync::oneshot::channel::<()>();
        let first = tokio::spawn({
            let (executor, events) = (executor.clone(), events.clone());
            async move {
                executor
                    .with_lock("thread".to_string(), async move {
                        events.lock().unwrap().push("first:start");
                        entered_tx.send(()).unwrap();
                        release.await.unwrap();
                        events.lock().unwrap().push("first:end");
                    })
                    .await
            }
        });
        entered.await.unwrap();
        let second = tokio::spawn({
            let (executor, events) = (executor.clone(), events.clone());
            async move {
                executor
                    .with_lock("thread".to_string(), async move {
                        events.lock().unwrap().push("second:start");
                    })
                    .await
            }
        });
        tokio::task::yield_now().await;
        assert_eq!(*events.lock().unwrap(), ["first:start"]);
        release_tx.send(()).unwrap();
        first.await.unwrap();
        second.await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["first:start", "first:end", "second:start"]
        );
        assert_eq!(executor.tracked(), 0);
    }
}
