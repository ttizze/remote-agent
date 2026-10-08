//! Per-device registrations survive a phone connection closing, but expire with the activity.
use super::client::ResultKind;
use agent_protocol::{live_activity::RegisterLiveActivity, session::SessionRef};
use serde::Serialize;
use std::collections::HashMap;
use zeroize::Zeroizing;

const LIFETIME: u64 = 8 * 60 * 60;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Content {
    pub title: String,
    pub status: String,
    pub status_label: String,
    pub connected: bool,
    pub host_name: String,
}
struct Registration {
    session: SessionRef,
    token: Zeroizing<Vec<u8>>,
    content: Content,
    ongoing: bool,
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
}
impl Registry {
    pub fn register(
        &mut self,
        owner: &str,
        params: &RegisterLiveActivity,
        mut content: Content,
        mut ongoing: bool,
        now: u64,
    ) -> Result<(), &'static str> {
        params.validate()?;
        let key = (owner.to_owned(), params.activity_id.clone());
        if let Some(entry) = self.entries.get(&key) {
            if entry.session != params.session {
                return Err("Live Activity session cannot change");
            }
            if !entry.ongoing {
                content = entry.content.clone();
                ongoing = false;
            }
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
        let urgent = !ongoing || content.status == "waiting";
        self.generation += 1;
        self.entries.insert(
            key,
            Registration {
                session: params.session.clone(),
                token: Zeroizing::new(params.token.clone()),
                content,
                ongoing,
                expires,
                due: now.max(sent_at + 1),
                generation: self.generation,
                sent_at,
                urgent,
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
    pub fn update(&mut self, session: &SessionRef, phase: (&str, &str, bool), now: u64) {
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| &entry.session == session)
        {
            // Once this activity ends, a new run needs its own activity identity.
            if !entry.ongoing {
                continue;
            }
            let connected = phase.0 != "unknown";
            if entry.content.connected == connected
                && (!connected || (entry.content.status == phase.0 && entry.ongoing == phase.2))
            {
                continue;
            }
            if connected {
                entry.content.status = phase.0.into();
                entry.content.status_label = phase.1.into();
                entry.ongoing = phase.2;
            }
            entry.content.connected = connected;
            entry.urgent = !entry.ongoing || (connected && phase.0 == "waiting");
            self.generation += 1;
            entry.generation = self.generation;
            entry.due = now.max(entry.sent_at + 1);
        }
    }
    pub fn rename(&mut self, session: &SessionRef, title: &str, now: u64) {
        let title = agent_protocol::models::compact_title(title);
        for entry in self
            .entries
            .values_mut()
            .filter(|entry| &entry.session == session)
        {
            if entry.content.title == title {
                continue;
            }
            entry.content.title = title.clone();
            self.generation += 1;
            entry.generation = self.generation;
            entry.due = now.max(entry.sent_at + 1);
        }
    }
    pub fn deliveries(&mut self, now: u64) -> Vec<Delivery> {
        self.entries.retain(|_, entry| entry.expires > now);
        let mut deliveries = Vec::new();
        for ((owner, activity), entry) in &mut self.entries {
            if entry.due > now {
                continue;
            }
            entry.sent_at = now;
            let mut aps = serde_json::json!({
                "timestamp":now,
                "event":if entry.ongoing {"update"} else {"end"},
                "content-state":entry.content,
            });
            if entry.ongoing {
                aps["stale-date"] = serde_json::json!(if entry.content.connected {
                    now + 120
                } else {
                    now
                });
                aps["relevance-score"] = serde_json::json!(if entry.content.status == "waiting" {
                    100
                } else {
                    50
                });
            } else {
                aps["dismissal-date"] = serde_json::json!(now + 60);
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
            || (result == ResultKind::Accepted && !self.entries[&key].ongoing)
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
    fn params(activity: &str, token: u8) -> RegisterLiveActivity {
        RegisterLiveActivity {
            session: SessionRef {
                provider: ProviderKind::Codex,
                id: "task".into(),
            },
            activity_id: activity.into(),
            token: vec![token; 32],
            environment: PushEnvironment::Sandbox,
        }
    }
    fn content() -> Content {
        Content {
            title: "タスク".into(),
            status: "running".into(),
            status_label: "実行中".into(),
            connected: true,
            host_name: "PC".into(),
        }
    }
    fn aps(delivery: &Delivery) -> serde_json::Value {
        serde_json::from_slice::<serde_json::Value>(&delivery.payload).unwrap()["aps"].clone()
    }
    #[test]
    fn rotation_replaces_only_its_device_and_stale_completions_cannot_remove_new_tokens() {
        let mut registry = Registry::default();
        registry
            .register("phone", &params("activity", 1), content(), true, 100)
            .unwrap();
        registry
            .register("other", &params("activity", 2), content(), true, 100)
            .unwrap();
        let old = registry
            .deliveries(100)
            .into_iter()
            .find(|d| d.owner == "phone")
            .unwrap();
        registry
            .register("phone", &params("activity", 3), content(), true, 101)
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
        assert_eq!(registry.entries.len(), 1);
        registry.revoke("phone");
        assert!(registry.deliveries(101).is_empty());
    }
    #[test]
    fn terminal_push_contains_final_content_and_is_not_replaced_by_later_idle_or_rotation() {
        for phase in [
            ("completed", "完了", false),
            ("failed", "失敗", false),
            ("interrupted", "中断", false),
        ] {
            let mut registry = Registry::default();
            let params = params("activity", 1);
            registry
                .register("phone", &params, content(), true, 100)
                .unwrap();
            registry.update(&params.session, phase, 101);
            registry.update(&params.session, ("finished", "終了", false), 102);
            registry
                .register("phone", &params, content(), true, 102)
                .unwrap();
            let delivery = registry.deliveries(102).remove(0);
            assert!(delivery.urgent);
            assert_eq!(
                aps(&delivery),
                serde_json::json!({"timestamp":102,"event":"end","dismissal-date":162,"content-state":{"title":"タスク","status":phase.0,"statusLabel":phase.1,"connected":true,"hostName":"PC"}})
            );
            registry.complete(&delivery, ResultKind::Retry, 102);
            assert!(registry.deliveries(111).is_empty());
            let retry = registry.deliveries(112).remove(0);
            registry.complete(&retry, ResultKind::Accepted, 112);
            assert!(registry.deliveries(200).is_empty());
        }
    }
    #[test]
    fn heartbeat_unknown_state_and_title_changes_do_not_falsely_end_or_flood_updates() {
        let mut registry = Registry::default();
        let params = params("activity", 1);
        registry
            .register("phone", &params, content(), true, 100)
            .unwrap();
        let first = registry.deliveries(100).remove(0);
        assert!(!first.urgent);
        assert_eq!(aps(&first)["stale-date"], 220);
        registry.complete(&first, ResultKind::Accepted, 100);
        registry.rename(&params.session, "タスク", 101);
        assert!(registry.deliveries(159).is_empty());
        let heartbeat = registry.deliveries(160).remove(0);
        registry.complete(&heartbeat, ResultKind::Accepted, 160);
        registry.update(&params.session, ("unknown", "更新待ち", false), 161);
        let stale = registry.deliveries(161).remove(0);
        assert_eq!(aps(&stale)["event"], "update");
        assert_eq!(aps(&stale)["content-state"]["status"], "running");
        assert_eq!(aps(&stale)["content-state"]["connected"], false);
        assert_eq!(aps(&stale)["stale-date"], 161);
        registry.complete(&stale, ResultKind::Accepted, 161);
        registry.update(&params.session, ("unknown", "更新待ち", false), 162);
        assert!(registry.deliveries(162).is_empty());
        registry.rename(&params.session, "新しい名前\n本文", 163);
        assert_eq!(
            aps(&registry.deliveries(163).remove(0))["content-state"]["title"],
            "新しい名前"
        );
        registry.update(&params.session, ("waiting", "確認待ち", true), 164);
        let waiting = registry.deliveries(164).remove(0);
        assert!(waiting.urgent);
        assert_eq!(aps(&waiting)["relevance-score"], 100);
        assert_eq!(aps(&waiting)["content-state"]["connected"], true);
        registry.complete(&waiting, ResultKind::Accepted, 164);
        assert!(!registry.deliveries(224).remove(0).urgent);
    }
    #[test]
    fn expired_tokens_capacity_and_activity_lifetime_are_bounded() {
        let mut registry = Registry::default();
        for n in 0..16 {
            registry
                .register("phone", &params(&n.to_string(), 1), content(), true, 100)
                .unwrap();
        }
        assert!(
            registry
                .register("phone", &params("17", 1), content(), true, 100)
                .is_err()
        );
        registry
            .register("phone", &params("0", 2), content(), true, 200)
            .unwrap();
        let delivery = registry
            .deliveries(200)
            .into_iter()
            .find(|d| d.activity == "0")
            .unwrap();
        registry.complete(&delivery, ResultKind::Expired, 200);
        assert_eq!(registry.entries.len(), 15);
        assert_eq!(registry.deliveries(100 + LIFETIME - 1).len(), 15);
        assert!(registry.deliveries(100 + LIFETIME).is_empty());
    }
    #[test]
    fn failed_pushes_back_off_and_success_restores_the_normal_heartbeat() {
        let mut registry = Registry::default();
        registry
            .register("phone", &params("activity", 1), content(), true, 100)
            .unwrap();
        let mut time = 100;
        for delay in [10, 20, 40, 80, 160, 300, 300] {
            let delivery = registry.deliveries(time).remove(0);
            registry.complete(&delivery, ResultKind::Retry, time);
            assert!(registry.deliveries(time + delay - 1).is_empty());
            time += delay;
        }
        let delivery = registry.deliveries(time).remove(0);
        registry.complete(&delivery, ResultKind::Accepted, time);
        assert!(registry.deliveries(time + 59).is_empty());
        let heartbeat = registry.deliveries(time + 60).remove(0);
        registry.complete(&heartbeat, ResultKind::Retry, time + 60);
        assert!(registry.deliveries(time + 69).is_empty());
        assert_eq!(registry.deliveries(time + 70).len(), 1);
    }
    #[test]
    fn provider_identity_and_global_capacity_prevent_cross_task_updates_and_unbounded_registrations()
     {
        let mut registry = Registry::default();
        let params = params("activity", 1);
        registry
            .register("phone", &params, content(), true, 100)
            .unwrap();
        let mut other_provider = params.clone();
        other_provider.session.provider = ProviderKind::Claude;
        assert!(
            registry
                .register("phone", &other_provider, content(), true, 100)
                .is_err()
        );
        registry.update(&other_provider.session, ("failed", "失敗", false), 101);
        assert_eq!(
            aps(&registry.deliveries(101).remove(0))["content-state"]["status"],
            "running"
        );
        registry.revoke("phone");
        for owner in 0..16 {
            for activity in 0..16 {
                let mut registration = params.clone();
                registration.activity_id = activity.to_string();
                registry
                    .register(&owner.to_string(), &registration, content(), true, 100)
                    .unwrap();
            }
        }
        assert!(
            registry
                .register("new-phone", &params, content(), true, 100)
                .is_err()
        );
        registry.unregister("0", "0");
        registry
            .register("new-phone", &params, content(), true, 100)
            .unwrap();
        assert_eq!(registry.deliveries(100).len(), 256);
    }
    proptest::proptest! {
        #[test]
        fn revoke_and_unregister_never_touch_another_devices_registration(operations in proptest::collection::vec((proptest::bool::ANY, 0u8..8), 0..80)) {
            let mut registry = Registry::default();
            for id in 0..8 { registry.register("other", &params(&id.to_string(), 2), content(), true, 100).unwrap(); }
            for (remove, id) in operations {
                let activity = id.to_string();
                if remove { registry.unregister("phone", &activity); } else { registry.register("phone", &params(&activity, 1), content(), true, 100).unwrap(); }
                registry.revoke("phone");
                proptest::prop_assert_eq!(registry.deliveries(100).len(), 8);
                proptest::prop_assert!(registry.entries.keys().all(|(owner, _)| owner == "other"));
            }
        }
    }
}
