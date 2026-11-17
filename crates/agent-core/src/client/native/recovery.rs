//! One retry owner per native Store; platforms supply lifecycle wakeups only.
use crate::{
    store::Store,
    transport::{Endpoint, Ticket, TransportError},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

pub(super) struct Recovery {
    wake: Arc<Notify>,
    _stop: DropGuard,
}
impl Recovery {
    pub(super) fn start(
        store: Arc<Store>,
        endpoint: Endpoint,
        ticket: Ticket,
        device_name: String,
        client_revision: String,
        attempt: Arc<tokio::sync::Mutex<()>>,
        mut lifecycle: watch::Receiver<bool>,
    ) -> Self {
        let stop = CancellationToken::new();
        let wake = Arc::new(Notify::new());
        let wakeup = wake.clone();
        let cancelled = stop.clone();
        tokio::spawn(async move {
            let mut snapshots = store.subscribe();
            let mut failures = 0;
            let mut connected_since = None;
            let mut unauthorized = false;
            let work = async {
                loop {
                    let connected = snapshots.borrow_and_update().connection.connected;
                    let enabled = snapshots.borrow().connection_enabled();
                    if connected {
                        connected_since.get_or_insert_with(Instant::now);
                    } else if connected_since
                        .take()
                        .is_some_and(|since| since.elapsed() >= Duration::from_secs(30))
                    {
                        failures = 0;
                    }
                    if connected || !enabled || !*lifecycle.borrow_and_update() || unauthorized {
                        tokio::select! {
                            result = snapshots.changed() => if result.is_err() { return; },
                            result = lifecycle.changed() => { if result.is_err() { return; } unauthorized = false; },
                            _ = wakeup.notified() => unauthorized = false,
                        }
                        continue;
                    }
                    let entropy = u16::from_le_bytes(
                        uuid::Uuid::new_v4().as_bytes()[..2].try_into().unwrap(),
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(retry_delay(failures, entropy)) => {},
                        _ = wakeup.notified() => {},
                        result = lifecycle.changed() => { if result.is_err() { return; } continue; },
                    }
                    let _attempt = attempt.lock().await;
                    if store.snapshot().connection.connected
                        || !store.snapshot().connection_enabled()
                        || !*lifecycle.borrow()
                    {
                        continue;
                    }
                    match store.resume(&endpoint, &ticket, &device_name).await {
                        Ok(performance) => {
                            store.record_connection_performance(performance.with_client(
                                &client_revision,
                                crate::diagnostics::ClientPlatform::current(),
                            ));
                        }
                        Err(TransportError::Unauthorized | TransportError::Ticket(_)) => {
                            unauthorized = true
                        }
                        Err(_) => failures = failures.saturating_add(1),
                    }
                }
            };
            tokio::select! { biased; _ = cancelled.cancelled() => {}, _ = work => {} }
        });
        Self {
            wake,
            _stop: stop.drop_guard(),
        }
    }
    pub(super) fn wake(&self) {
        self.wake.notify_one();
    }
}
fn retry_delay(failures: u32, entropy: u16) -> Duration {
    let ceiling = (2u64.saturating_pow(failures.min(8)) * 2).min(300);
    Duration::from_millis(ceiling * 500 + u64::from(entropy) * ceiling * 500 / u64::from(u16::MAX))
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[rstest::rstest]
    #[case(0, 1, 2)]
    #[case(5, 32, 64)]
    #[case(8, 150, 300)]
    #[case(u32::MAX, 150, 300)]
    fn retry_backoff_grows_then_caps(
        #[case] failures: u32,
        #[case] floor: u64,
        #[case] ceiling: u64,
    ) {
        assert_eq!(retry_delay(failures, 0), Duration::from_secs(floor));
        assert_eq!(
            retry_delay(failures, u16::MAX),
            Duration::from_secs(ceiling)
        );
    }
    proptest! {
        #[test]
        fn retries_are_bounded_and_spread(failures in any::<u32>(), entropy in any::<u16>()) {
            let delay = retry_delay(failures, entropy);
            prop_assert!((Duration::from_secs(1)..=Duration::from_secs(300)).contains(&delay));
            prop_assert!(retry_delay(failures, 0) < retry_delay(failures, u16::MAX));
        }
    }
}
