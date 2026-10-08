//! Per-device registrations survive a phone connection closing, but expire with the activity.
use super::client::ResultKind;
use agent_core::presentation::task_activity::task_activity_timing;
use agent_protocol::{
    live_activity::{RegisterLiveActivity, TaskActivityDisplay, TaskActivitySummary},
    session::SessionRef,
};
use serde::Serialize;
use std::collections::HashMap;
use zeroize::Zeroizing;

const LIFETIME: u64 = 8 * 60 * 60;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Content {
    pub display: TaskActivityDisplay,
    pub connected: bool,
    pub host_name: String,
}
struct Registration {
    token: Zeroizing<Vec<u8>>,
    content: Content,
    expires: u64,
    due: u64,
    generation: u64,
    sent_at: u64,
    urgent: bool,
    retry_delay: u64,
}
pub(super) struct Delivery {
    pub owner: String,
    pub activity: String,
    pub token: Zeroizing<Vec<u8>>,
    pub payload: Vec<u8>,
    pub urgent: bool,
    generation: u64,
}
#[derive(Default)]
pub(super) struct Registry {
    entries: HashMap<(String, String), Registration>,
    generation: u64,
    tasks: HashMap<SessionRef, String>,
}
impl Registry {
    pub fn register(
        &mut self,
        owner: &str,
        params: &RegisterLiveActivity,
        host_name: &str,
        now: u64,
    ) -> Result<(), &'static str> {
        params.validate()?;
        let mut content = Content {
            display: self.summary().display(),
            connected: true,
            host_name: host_name.to_owned(),
        };
        let key = (owner.to_owned(), params.activity_id.clone());
        if let Some(entry) = self.entries.get(&key)
            && !entry.content.display.ongoing
        {
            content = entry.content.clone();
        }
        if !self.entries.contains_key(&key)
            && (self.entries.len() >= 256
                || self
                    .entries
                    .keys()
                    .filter(|(principal, _)| principal == owner)
                    .count()
                    >= 16)
        {
            return Err("Live Activity registration capacity reached");
        }
        // Token rotation replaces only the same authenticated device's registration.
        let expires = self
            .entries
            .get(&key)
            .map_or(now + LIFETIME, |entry| entry.expires);
        let sent_at = self.entries.get(&key).map_or(0, |entry| entry.sent_at);
        self.generation += 1;
        self.entries.insert(
            key,
            Registration {
                token: Zeroizing::new(params.token.clone()),
                content,
                expires,
                due: now.max(sent_at + 1),
                generation: self.generation,
                sent_at,
                // Establish delivery promptly before relying on low-priority heartbeats.
                urgent: true,
                retry_delay: 0,
            },
        );
        Ok(())
    }
    pub fn unregister(&mut self, owner: &str, activity: &str) {
        self.entries
            .remove(&(owner.to_owned(), activity.to_owned()));
    }
    pub fn revoke(&mut self, owner: &str) {
        self.entries.retain(|(principal, _), _| principal != owner);
    }
    fn summary(&self) -> TaskActivitySummary {
        TaskActivitySummary::from_statuses(self.tasks.values().map(String::as_str))
    }
    pub fn seed(&mut self, tasks: Vec<(SessionRef, String)>, now: u64) {
        // Changes received while the native list was loading are authoritative.
        for (session, status) in tasks {
            if status != "unknown"
                && TaskActivitySummary::from_statuses([status.as_str()]).ongoing()
            {
                self.tasks.entry(session).or_insert(status);
            }
        }
        self.refresh(now);
    }
    pub fn update(&mut self, session: &SessionRef, next_status: &str, now: u64) {
        if self
            .tasks
            .get(session)
            .is_some_and(|status| status == next_status)
        {
            return;
        }
        if next_status == "unknown"
            && !self.tasks.get(session).is_some_and(|status| {
                TaskActivitySummary::from_statuses([status.as_str()]).ongoing()
            })
        {
            return;
        }
        self.tasks.insert(session.clone(), next_status.into());
        self.refresh(now);
    }
    fn refresh(&mut self, now: u64) {
        let display = self.summary().display();
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| entry.content.display.ongoing)
        {
            if entry.content.display == display {
                continue;
            }
            entry.content.display = display.clone();
            entry.urgent = display.urgent;
            self.generation += 1;
            entry.generation = self.generation;
            entry.due = now.max(entry.sent_at + 1);
        }
    }
    pub fn deliveries(&mut self, now: u64) -> Vec<Delivery> {
        self.entries.retain(|_, entry| entry.expires > now);
        let mut deliveries = Vec::new();
        let timing = task_activity_timing(false, true);
        for ((owner, activity), entry) in &mut self.entries {
            if entry.due > now {
                continue;
            }
            entry.sent_at = now;
            let mut aps = serde_json::json!({
                "timestamp":now,
                "event":if entry.content.display.ongoing {"update"} else {"end"},
                "content-state":entry.content,
            });
            if entry.content.display.ongoing {
                aps["stale-date"] = serde_json::json!(
                    now + u64::from(
                        timing
                            .stale_after_seconds
                            .expect("remote updates have a freshness limit")
                    )
                );
                aps["relevance-score"] = serde_json::json!(if entry.content.display.urgent {
                    100
                } else {
                    50
                });
            } else {
                aps["dismissal-date"] =
                    serde_json::json!(now + u64::from(timing.dismiss_after_seconds));
            }
            deliveries.push(Delivery {
                owner: owner.clone(),
                activity: activity.clone(),
                token: entry.token.clone(),
                payload: serde_json::to_vec(&serde_json::json!({"aps":aps}))
                    .expect("bounded APNs content encodes"),
                urgent: entry.urgent,
                generation: entry.generation,
            });
        }
        deliveries
    }
    pub fn current(&self, delivery: &Delivery) -> bool {
        self.entries
            .get(&(delivery.owner.clone(), delivery.activity.clone()))
            .is_some_and(|entry| entry.generation == delivery.generation)
    }
    pub fn complete(&mut self, delivery: &Delivery, result: ResultKind, now: u64) {
        if !self.current(delivery) {
            return;
        }
        let key = (delivery.owner.clone(), delivery.activity.clone());
        if result == ResultKind::Expired
            || (result == ResultKind::Accepted && !self.entries[&key].content.display.ongoing)
        {
            self.entries.remove(&key);
        } else if let Some(entry) = self.entries.get_mut(&key) {
            if result == ResultKind::Retry {
                entry.retry_delay = (entry.retry_delay * 2).clamp(10, 300);
                entry.due = now + entry.retry_delay;
            } else {
                entry.retry_delay = 0;
                entry.due = now + 60;
                entry.urgent = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{live_activity::PushEnvironment, session::ProviderKind};
    fn session(provider: ProviderKind) -> SessionRef {
        SessionRef {
            provider,
            id: "task".into(),
        }
    }
    fn params(activity: &str, token: u8) -> RegisterLiveActivity {
        RegisterLiveActivity {
            activity_id: activity.into(),
            token: vec![token; 32],
            environment: PushEnvironment::Sandbox,
        }
    }
    fn registry() -> Registry {
        let mut registry = Registry::default();
        registry.update(&session(ProviderKind::Codex), "running", 100);
        registry
    }
    fn aps(delivery: &Delivery) -> serde_json::Value {
        serde_json::from_slice::<serde_json::Value>(&delivery.payload).unwrap()["aps"].clone()
    }
    #[test]
    fn rotation_is_owned_by_its_device_and_fences_inflight_deliveries() {
        let mut registry = registry();
        registry
            .register("phone", &params("activity", 1), "PC", 100)
            .unwrap();
        registry
            .register("other", &params("activity", 2), "PC", 100)
            .unwrap();
        let old = registry
            .deliveries(100)
            .into_iter()
            .find(|d| d.owner == "phone")
            .unwrap();
        registry
            .register("phone", &params("activity", 3), "PC", 101)
            .unwrap();
        assert!(!registry.current(&old));
        registry.complete(&old, ResultKind::Expired, 101);
        assert_eq!(registry.entries.len(), 2);
        assert_eq!(
            registry.entries[&("phone".into(), "activity".into())]
                .token
                .as_slice(),
            &[3; 32]
        );
        registry.unregister("other", "activity");
        registry.revoke("phone");
        assert!(registry.deliveries(101).is_empty());
    }
    #[test]
    fn all_providers_share_one_activity_and_only_the_last_task_ends_it() {
        let mut registry = registry();
        let codex = session(ProviderKind::Codex);
        let claude = session(ProviderKind::Claude);
        registry.update(&claude, "waiting", 100);
        registry
            .register("phone", &params("activity", 1), "PC", 100)
            .unwrap();
        let first = registry.deliveries(100).remove(0);
        assert_eq!(
            aps(&first)["content-state"]["display"],
            serde_json::to_value(
                TaskActivitySummary {
                    running: 1,
                    waiting: 1,
                    unknown: 0
                }
                .display()
            )
            .unwrap()
        );
        assert!(first.urgent);
        registry.complete(&first, ResultKind::Accepted, 100);
        registry.update(&codex, "failed", 101);
        let remaining = registry.deliveries(101).remove(0);
        assert_eq!(aps(&remaining)["event"], "update");
        assert_eq!(
            aps(&remaining)["content-state"]["display"]["current"]["label"],
            "確認待ち 1件"
        );
        registry.complete(&remaining, ResultKind::Accepted, 101);
        registry.update(&claude, "completed", 102);
        registry.update(&claude, "running", 103);
        registry
            .register("phone", &params("activity", 2), "PC", 103)
            .unwrap();
        let final_push = registry.deliveries(103).remove(0);
        assert_eq!(aps(&final_push)["event"], "end");
        assert_eq!(aps(&final_push)["dismissal-date"], 163);
        registry.complete(&final_push, ResultKind::Retry, 103);
        assert!(registry.deliveries(112).is_empty());
        let retry = registry.deliveries(113).remove(0);
        registry.complete(&retry, ResultKind::Accepted, 113);
        assert!(registry.deliveries(200).is_empty());
    }
    #[test]
    fn unknown_state_retains_an_active_icon_without_ending_or_flooding() {
        let mut registry = registry();
        registry
            .register("phone", &params("activity", 1), "PC", 100)
            .unwrap();
        let first = registry.deliveries(100).remove(0);
        assert!(first.urgent);
        assert_eq!(aps(&first)["stale-date"], 700);
        registry.complete(&first, ResultKind::Accepted, 100);
        assert!(registry.deliveries(159).is_empty());
        let heartbeat = registry.deliveries(160).remove(0);
        assert!(!heartbeat.urgent);
        registry.complete(&heartbeat, ResultKind::Accepted, 160);
        let codex = session(ProviderKind::Codex);
        registry.update(&codex, "unknown", 161);
        let stale = registry.deliveries(161).remove(0);
        assert_eq!(aps(&stale)["event"], "update");
        assert_eq!(
            aps(&stale)["content-state"]["display"]["current"]["label"],
            "状態確認中 1件"
        );
        assert_eq!(aps(&stale)["stale-date"], 761);
        registry.complete(&stale, ResultKind::Accepted, 161);
        registry.update(&codex, "unknown", 162);
        assert!(registry.deliveries(162).is_empty());
        registry.seed(vec![(codex.clone(), "running".into())], 163);
        assert_eq!(registry.summary().unknown, 1);
        registry.update(&codex, "waiting", 164);
        let waiting = registry.deliveries(164).remove(0);
        assert_eq!(aps(&waiting)["relevance-score"], 100);
        registry.complete(&waiting, ResultKind::Accepted, 164);
        assert!(!registry.deliveries(224).remove(0).urgent);
    }
    #[test]
    fn registration_capacity_expiry_and_payload_size_are_bounded() {
        let mut registry = registry();
        for i in 0..16 {
            registry
                .register("phone", &params(&i.to_string(), 1), "PC", 100)
                .unwrap();
        }
        assert!(
            registry
                .register("phone", &params("overflow", 1), "PC", 100)
                .is_err()
        );
        for i in 1..16 {
            for activity in 0..16 {
                registry
                    .register(
                        &format!("phone{i}"),
                        &params(&activity.to_string(), 1),
                        "PC",
                        100,
                    )
                    .unwrap();
            }
        }
        assert!(
            registry
                .register("last", &params("overflow", 1), "PC", 100)
                .is_err()
        );
        registry
            .register("phone", &params("0", 2), "PC", 100 + LIFETIME - 1)
            .unwrap();
        assert!(registry.deliveries(100 + LIFETIME).is_empty());
        registry
            .register("phone", &params("new", 1), "PC", 100 + LIFETIME)
            .unwrap();
        let delivery = registry.deliveries(100 + LIFETIME).remove(0);
        registry.complete(&delivery, ResultKind::Expired, 100 + LIFETIME);
        assert!(registry.entries.is_empty());
    }
    proptest::proptest! {
        #[test]
        fn counts_fit_push_payload_even_with_many_tasks(count in 1u32..10000) {
            let mut registry = Registry::default();
            registry.seed((0..count).map(|i| (SessionRef { provider: ProviderKind::Codex, id:i.to_string() }, "running".into())).collect(), 100);
            registry.register("phone", &params("activity", 1), "PC", 100).unwrap();
            let delivery = registry.deliveries(100).remove(0);
            proptest::prop_assert!(delivery.payload.len() < 4096);
            proptest::prop_assert_eq!(aps(&delivery)["content-state"]["display"]["current"]["total"].as_u64(), Some(u64::from(count)));
        }
    }
}
