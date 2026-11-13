//! Host-owned Live Activity push updates, independent of connected phone sessions.
mod client;
mod registry;
use agent_protocol::live_activity::{RegisterLiveActivity, TaskActivityDisplay};
use client::{Client, ResultKind};
use futures_util::{StreamExt, stream};
use registry::Registry;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
        let mut registry = Registry::default();
        registry.update(
            agent_protocol::live_activity::TaskActivitySummary::default().display(),
            now(),
        );
        Arc::new(Self {
            client: Client::testing(),
            host_name: "PC".into(),
            registry: Mutex::new(registry),
            path: None,
            wake: Arc::new(Notify::new()),
            stop: CancellationToken::new(),
        })
    }
    #[cfg(test)]
    pub(crate) fn test_payloads(&self, timestamp: u64) -> Vec<serde_json::Value> {
        let mut registry = self.registry.lock().unwrap();
        if !registry.is_ready() {
            return Vec::new();
        }
        registry
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
        let registry_path = path.with_file_name("bex-live-activities.json");
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
                let deliveries = {
                    let mut registry = service
                        .registry
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if !registry.is_ready() {
                        continue;
                    }
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
                                .send(
                                    delivery.environment,
                                    &delivery.token,
                                    &delivery.payload,
                                    delivery.urgent,
                                    now(),
                                )
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
            agent_local_host::save_private_json(path, registry)?;
        }
        Ok(())
    }
    fn persist(&self, registry: &Registry) {
        if self.save(registry).is_err() {
            tracing::error!(target: "agent", operation = "host.live_activity.persist", message = "cannot persist Live Activity registrations");
        }
    }
    pub fn activate(&self, display: TaskActivityDisplay) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let was_ready = registry.is_ready();
        let changed = registry.update(display, now());
        if changed || !was_ready {
            self.persist(&registry);
            self.wake.notify_one();
        }
    }
    pub fn register(&self, owner: &str, params: &RegisterLiveActivity) -> Result<(), String> {
        if !self.client.credential_available(params.environment) {
            return Err(format!(
                "APNs {:?} credentials are not configured",
                params.environment
            ));
        }
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry
            .register(owner, params, &self.host_name, now())
            .map_err(str::to_owned)?;
        self.save(&registry)
            .map_err(|_| "cannot persist Live Activity registration".to_owned())?;
        self.wake.notify_one();
        Ok(())
    }
    pub fn update(&self, display: TaskActivityDisplay) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !registry.is_ready() {
            return;
        }
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
    use agent_protocol::live_activity::{PushEnvironment, TaskActivitySummary};
    #[tokio::test]
    async fn unsupported_signature_environment_is_rejected_before_registration() {
        use base64::Engine;
        use ring::{
            rand::SystemRandom,
            signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
        };
        let directory = tempfile::tempdir().unwrap();
        let key =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(key.as_ref())
        );
        tokio::fs::write(directory.path().join("sandbox.p8"), pem)
            .await
            .unwrap();
        let path = directory.path().join("bex-apns.json");
        tokio::fs::write(&path, br#"{"team_id":"TESTTEAM01","bundle_id":"dev.remoteagent.mobile.ios.dev","sandbox":{"key_id":"TESTKEY001","key_file":"sandbox.p8"}}"#).await.unwrap();
        let mut service = Apns::testing();
        Arc::get_mut(&mut service).unwrap().client = Client::load(&path).await.unwrap().unwrap();
        let mut params = RegisterLiveActivity {
            activity_id: Some("dev-activity".into()),
            allow_start: false,
            token: vec![1; 32],
            environment: PushEnvironment::Production,
        };
        let display = TaskActivitySummary {
            running: 1,
            ..Default::default()
        }
        .display();
        service.activate(display);
        let error = service.register("phone", &params).unwrap_err();
        assert!(error.contains("Production"));
        assert!(service.test_payloads(now()).is_empty());
        params.environment = PushEnvironment::Sandbox;
        service.register("phone", &params).unwrap();
        assert_eq!(service.test_payloads(now()).len(), 1);
    }

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
        service.activate(running);
        service
            .register(
                "phone",
                &RegisterLiveActivity {
                    activity_id: Some("activity".into()),
                    allow_start: false,
                    token: vec![1; 32],
                    environment: PushEnvironment::Sandbox,
                },
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
    #[test]
    fn registering_after_a_display_update_uses_the_latest_state_even_without_existing_entries() {
        for summary in [
            TaskActivitySummary {
                waiting: 1,
                ..Default::default()
            },
            TaskActivitySummary::default(),
        ] {
            let service = Apns::testing();
            service.activate(
                TaskActivitySummary {
                    running: 1,
                    ..Default::default()
                }
                .display(),
            );
            let latest = summary.display();
            service.update(latest.clone());
            service
                .register(
                    "phone",
                    &RegisterLiveActivity {
                        activity_id: Some("late".into()),
                        allow_start: false,
                        token: vec![1; 32],
                        environment: PushEnvironment::Sandbox,
                    },
                )
                .unwrap();
            let payload = service.test_payloads(now()).pop().unwrap();
            assert_eq!(
                payload["aps"]["content-state"]["display"],
                serde_json::to_value(latest).unwrap()
            );
        }
    }
}
