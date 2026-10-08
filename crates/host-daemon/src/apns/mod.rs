//! Host-owned Live Activity push updates, independent of connected phone sessions.
mod client;
mod registry;
use agent_protocol::{
    live_activity::{PushEnvironment, RegisterLiveActivity},
    session::SessionRef,
};
use client::{Client, ResultKind};
use futures_util::{StreamExt, stream};
use registry::{Content, Registry};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub(crate) struct Apns {
    client: Client,
    host_name: String,
    registry: Mutex<Registry>,
    wake: Arc<Notify>,
    stop: CancellationToken,
}
impl Drop for Apns {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Apns {
    #[cfg(test)]
    pub(crate) fn testing() -> Arc<Self> {
        Arc::new(Self {
            client: Client::testing(),
            host_name: "PC".into(),
            registry: Mutex::default(),
            wake: Arc::new(Notify::new()),
            stop: CancellationToken::new(),
        })
    }
    #[cfg(test)]
    pub(crate) fn test_payloads(&self, timestamp: u64) -> Vec<serde_json::Value> {
        self.registry
            .lock()
            .unwrap()
            .deliveries(timestamp)
            .into_iter()
            .map(|delivery| serde_json::from_slice(&delivery.payload).unwrap())
            .collect()
    }
    pub async fn load(path: &Path, host_name: &str) -> anyhow::Result<Option<Arc<Self>>> {
        let Some(client) = Client::load(path).await? else {
            return Ok(None);
        };
        let service = Arc::new(Self {
            client,
            host_name: agent_protocol::models::compact_title(host_name),
            registry: Mutex::default(),
            wake: Arc::new(Notify::new()),
            stop: CancellationToken::new(),
        });
        let weak = Arc::downgrade(&service);
        let wake = service.wake.clone();
        let stop = service.stop.clone();
        tokio::spawn(async move {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! { biased; _ = stop.cancelled() => break, _ = wake.notified() => {}, _ = heartbeat.tick() => {} }
                let Some(service) = weak.upgrade() else {
                    break;
                };
                let deliveries = service
                    .registry
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .deliveries(now());
                let service = &service;
                stream::iter(deliveries)
                    .for_each_concurrent(4, |delivery| async move {
                        if !service
                            .registry
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .current(&delivery)
                        {
                            return;
                        }
                        let result = if delivery.payload.len() <= 4096 {
                            service
                                .client
                                .send(&delivery.token, &delivery.payload, delivery.urgent, now())
                                .await
                        } else {
                            ResultKind::Expired
                        };
                        service
                            .registry
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .complete(&delivery, result, now());
                    })
                    .await;
            }
        });
        Ok(Some(service))
    }
    pub fn environment(&self) -> PushEnvironment {
        self.client.environment()
    }
    pub fn register(
        &self,
        owner: &str,
        params: &RegisterLiveActivity,
        tasks: Vec<(SessionRef, String)>,
    ) -> Result<(), &'static str> {
        let content = Content {
            summary: Default::default(),
            connected: true,
            host_name: self.host_name.clone(),
        };
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let timestamp = now();
        registry.seed(tasks, timestamp);
        registry.register(owner, params, content, timestamp)?;
        self.wake.notify_one();
        Ok(())
    }
    pub fn update(&self, session: &SessionRef, status: &str) {
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .update(session, status, now());
        self.wake.notify_one();
    }
    pub fn unregister(&self, owner: &str, activity: &str) {
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unregister(owner, activity);
    }
    pub fn revoke(&self, owner: &str) {
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .revoke(owner);
    }
}
