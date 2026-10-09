//! Host-owned Live Activity push updates, independent of connected phone sessions.
mod client;
mod registry;
use agent_protocol::live_activity::{PushEnvironment, RegisterLiveActivity, TaskActivityDisplay};
use client::{Client, ResultKind};
use futures_util::{StreamExt, stream};
use registry::Registry;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub(crate) struct Apns {
    client: Client,
    host_name: String,
    registry: Mutex<Registry>,
    path: Option<PathBuf>,
    ready: AtomicBool,
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
            path: None,
            ready: AtomicBool::new(true),
            wake: Arc::new(Notify::new()),
            stop: CancellationToken::new(),
        })
    }
    #[cfg(test)]
    pub(crate) fn test_payloads(&self, timestamp: u64) -> Vec<serde_json::Value> {
        if !self.is_ready() {
            return Vec::new();
        }
        self.registry
            .lock()
            .unwrap()
            .deliveries(timestamp)
            .into_iter()
            .map(|delivery| serde_json::from_slice(&delivery.payload).unwrap())
            .collect()
    }
    pub async fn load(
        path: &Path,
        host_name: &str,
        host_id: &str,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        let Some(client) = Client::load(path).await? else {
            return Ok(None);
        };
        let registry_path = path.with_file_name(match client.environment() {
            PushEnvironment::Sandbox => "bex-live-activities-sandbox.json",
            PushEnvironment::Production => "bex-live-activities-production.json",
        });
        let mut registry: Registry = match tokio::fs::read(&registry_path).await {
            Ok(bytes) => serde_json::from_slice(&Zeroizing::new(bytes))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(error) => return Err(error.into()),
        };
        if registry.host_id != host_id {
            registry = Registry::default();
        }
        registry.host_id = host_id.to_owned();
        let service = Arc::new(Self {
            client,
            host_name: agent_protocol::models::compact_title(host_name),
            registry: Mutex::new(registry),
            path: Some(registry_path),
            ready: AtomicBool::new(false),
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
                if !service.is_ready() {
                    continue;
                }
                let deliveries = {
                    let mut registry = service
                        .registry
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let deliveries = registry.deliveries(now());
                    if !deliveries.is_empty() {
                        service.persist(&registry);
                    }
                    deliveries
                };
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
                        let mut registry = service
                            .registry
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        registry.complete(&delivery, result, now());
                        service.persist(&registry);
                    })
                    .await;
            }
        });
        Ok(Some(service))
    }
    fn save(&self, registry: &Registry) -> anyhow::Result<()> {
        if let Some(path) = &self.path {
            crate::platform::save_private_json(path, registry)?;
        }
        Ok(())
    }
    fn persist(&self, registry: &Registry) {
        if self.save(registry).is_err() {
            tracing::error!(target: "bex", operation = "host.live_activity.persist", message = "cannot persist Live Activity registrations");
        }
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
    pub fn activate(&self, display: TaskActivityDisplay) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let changed = registry.update(display, now());
        if changed || !self.is_ready() {
            self.persist(&registry);
            self.wake.notify_one();
        }
        self.ready.store(true, Ordering::Release);
    }
    pub fn environment(&self) -> PushEnvironment {
        self.client.environment()
    }
    pub fn register(
        &self,
        owner: &str,
        params: &RegisterLiveActivity,
        display: TaskActivityDisplay,
    ) -> Result<(), String> {
        if !self.is_ready() {
            return Err("Host task state is still initializing".into());
        }
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry
            .register(owner, params, &self.host_name, display, now())
            .map_err(str::to_owned)?;
        self.save(&registry)
            .map_err(|_| "cannot persist Live Activity registration".to_owned())?;
        self.wake.notify_one();
        Ok(())
    }
    pub fn update(&self, display: TaskActivityDisplay) {
        if !self.is_ready() {
            return;
        }
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if registry.update(display, now()) {
            self.persist(&registry);
            self.wake.notify_one();
        }
    }
    pub fn unregister(&self, owner: &str, activity: &str) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry.unregister(owner, activity);
        self.persist(&registry);
    }
    pub fn revoke(&self, owner: &str) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry.revoke(owner);
        self.persist(&registry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::live_activity::TaskActivitySummary;
    #[test]
    fn persisted_registrations_wait_for_fresh_host_state_and_retain_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("activities.json");
        let running = TaskActivitySummary {
            running: 1,
            ..Default::default()
        }
        .display();
        let mut service = Apns::testing();
        Arc::get_mut(&mut service).unwrap().path = Some(path.clone());
        service
            .register(
                "phone",
                &RegisterLiveActivity {
                    activity_id: Some("activity".into()),
                    allow_start: false,
                    token: vec![1; 32],
                    environment: PushEnvironment::Sandbox,
                },
                running.clone(),
            )
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut restarted = Apns::testing();
        let restarted_owner = Arc::get_mut(&mut restarted).unwrap();
        restarted_owner.path = Some(path.clone());
        restarted_owner.registry = Mutex::new(
            serde_json::from_slice(&Zeroizing::new(std::fs::read(&path).unwrap())).unwrap(),
        );
        restarted_owner.ready.store(false, Ordering::Release);
        assert!(restarted.test_payloads(now()).is_empty());
        restarted.activate(
            TaskActivitySummary {
                waiting: 1,
                ..Default::default()
            }
            .display(),
        );
        let deliveries = restarted.test_payloads(now());
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0]["aps"]["event"], "update");
        assert_eq!(
            deliveries[0]["aps"]["content-state"]["display"]["current"]["label"],
            "確認待ち 1件"
        );
        restarted.revoke("phone");
        let mut restored: Registry =
            serde_json::from_slice(&Zeroizing::new(std::fs::read(&path).unwrap())).unwrap();
        assert!(restored.deliveries(now() + 100).is_empty());
    }
}
