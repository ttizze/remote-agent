//! Device registrations and dismissal state are persisted by their Host owner.
use super::client::ResultKind;
use agent_protocol::live_activity::{
    RegisterLiveActivity, TASK_ACTIVITY_DISMISS_SECONDS, TaskActivityDisplay,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroize::Zeroizing;

const LIFETIME: u64 = 8 * 60 * 60;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Content {
    pub display: TaskActivityDisplay,
    pub host_name: String,
}
#[derive(Serialize, Deserialize)]
struct Registration {
    #[serde(with = "agent_protocol::protocol::bytes")]
    token: Zeroizing<Vec<u8>>,
    content: Content,
    expires: u64,
    due: u64,
    generation: u64,
    sent_at: u64,
    urgent: bool,
    retry_delay: u64,
    allow_start: bool,
    started: bool,
}
pub(super) struct Delivery {
    pub owner: String,
    pub activity: Option<String>,
    pub token: Zeroizing<Vec<u8>>,
    pub payload: Vec<u8>,
    pub urgent: bool,
    generation: u64,
}
type Registrations = HashMap<(String, Option<String>), Registration>;

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Registry {
    #[serde(with = "entries")]
    entries: Registrations,
    generation: u64,
    pub host_id: String,
}
mod entries {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        values: &Registrations,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(values.iter())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Registrations, D::Error> {
        let entries = Vec::<((String, Option<String>), Registration)>::deserialize(deserializer)?;
        let mut values = HashMap::new();
        for (key, value) in entries {
            if values.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("duplicate registration"));
            }
        }
        Ok(values)
    }
}
impl Registry {
    pub fn register(
        &mut self,
        owner: &str,
        params: &RegisterLiveActivity,
        host_name: &str,
        display: TaskActivityDisplay,
        now: u64,
    ) -> Result<(), &'static str> {
        params.validate()?;
        let key = (owner.to_owned(), params.activity_id.clone());
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
        let previous = self.entries.get(&key);
        let content = if params.activity_id.is_some()
            && previous.is_some_and(|entry| !entry.content.display.ongoing)
        {
            previous.unwrap().content.clone()
        } else {
            Content {
                display,
                host_name: host_name.to_owned(),
            }
        };
        let started = previous.is_some_and(|entry| entry.started);
        let expires = if params.activity_id.is_none() {
            u64::MAX
        } else {
            previous.map_or(now + LIFETIME, |entry| entry.expires)
        };
        let sent_at = previous.map_or(0, |entry| entry.sent_at);
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
                urgent: true,
                retry_delay: 0,
                allow_start: params.allow_start,
                started,
            },
        );
        if params.activity_id.is_some()
            && let Some(start) = self.entries.get_mut(&(owner.to_owned(), None))
        {
            start.started = true;
            self.generation += 1;
            start.generation = self.generation;
        }
        Ok(())
    }
    pub fn unregister(&mut self, owner: &str, activity: &str) {
        self.entries
            .remove(&(owner.to_owned(), Some(activity.to_owned())));
        // Dismissal suppresses another start until all tasks finish, including across a Host restart.
        if let Some(start) = self.entries.get_mut(&(owner.to_owned(), None)) {
            start.started = true;
            self.generation += 1;
            start.generation = self.generation;
        }
    }
    pub fn revoke(&mut self, owner: &str) {
        self.entries.retain(|(principal, _), _| principal != owner);
    }
    pub fn update(&mut self, display: TaskActivityDisplay, now: u64) -> bool {
        let previous = self.generation;
        for ((_, activity), entry) in &mut self.entries {
            if activity.is_some() && !entry.content.display.ongoing {
                continue;
            }
            if activity.is_none() && !display.ongoing {
                entry.started = false;
            }
            if entry.content.display == display {
                continue;
            }
            entry.urgent = true;
            entry.content.display = display.clone();
            self.generation += 1;
            entry.generation = self.generation;
            entry.due = now.max(entry.sent_at + 1);
        }
        self.generation != previous
    }
    pub fn deliveries(&mut self, now: u64) -> Vec<Delivery> {
        self.entries.retain(|_, entry| entry.expires > now);
        let active_owners: std::collections::HashSet<_> = self
            .entries
            .iter()
            .filter(|((_, id), entry)| id.is_some() && entry.content.display.ongoing)
            .map(|((owner, _), _)| owner.clone())
            .collect();
        let mut deliveries = Vec::new();
        for ((owner, activity), entry) in &mut self.entries {
            if entry.due > now
                || (activity.is_none()
                    && (!entry.allow_start
                        || entry.started
                        || !entry.content.display.can_start
                        || active_owners.contains(owner)))
            {
                continue;
            }
            entry.sent_at = now;
            let mut aps = serde_json::json!({
                "timestamp":now, "event":if entry.content.display.ongoing {"update"} else {"end"}, "content-state":entry.content,
            });
            if activity.is_none() {
                aps["event"] = serde_json::json!("start");
                aps["attributes-type"] = serde_json::json!("TaskActivityAttributes");
                aps["attributes"] = serde_json::json!({"hostID":self.host_id});
                aps["input-push-token"] = serde_json::json!(1);
                aps["alert"] = serde_json::json!({"title":entry.content.host_name, "body":entry.content.display.current.label});
            } else if entry.content.display.ongoing {
                aps["relevance-score"] = serde_json::json!(if entry.content.display.urgent {
                    100
                } else {
                    50
                });
            } else {
                aps["dismissal-date"] =
                    serde_json::json!(now + u64::from(TASK_ACTIVITY_DISMISS_SECONDS));
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
            || (result == ResultKind::Accepted
                && delivery.activity.is_some()
                && !self.entries[&key].content.display.ongoing)
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
                if delivery.activity.is_none() {
                    entry.started = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::live_activity::{PushEnvironment, TaskActivitySummary};
    fn params(activity: &str, token: u8) -> RegisterLiveActivity {
        RegisterLiveActivity {
            activity_id: Some(activity.into()),
            allow_start: false,
            token: vec![token; 32],
            environment: PushEnvironment::Sandbox,
        }
    }
    fn display(running: u32, waiting: u32, unknown: u32) -> TaskActivityDisplay {
        TaskActivitySummary {
            running,
            waiting,
            unknown,
        }
        .display()
    }
    fn aps(delivery: &Delivery) -> serde_json::Value {
        serde_json::from_slice::<serde_json::Value>(&delivery.payload).unwrap()["aps"].clone()
    }
    #[test]
    fn rotation_is_owned_by_its_device_and_fences_inflight_deliveries() {
        let mut registry = Registry::default();
        registry
            .register("phone", &params("activity", 1), "PC", display(1, 0, 0), 100)
            .unwrap();
        registry
            .register("other", &params("activity", 2), "PC", display(1, 0, 0), 100)
            .unwrap();
        let old = registry
            .deliveries(100)
            .into_iter()
            .find(|d| d.owner == "phone")
            .unwrap();
        registry
            .register("phone", &params("activity", 3), "PC", display(1, 0, 0), 101)
            .unwrap();
        assert!(!registry.current(&old));
        registry.complete(&old, ResultKind::Expired, 101);
        assert_eq!(registry.entries.len(), 2);
        assert_eq!(
            registry.entries[&("phone".into(), Some("activity".into()))]
                .token
                .as_slice(),
            &[3; 32]
        );
        registry.unregister("other", "activity");
        registry.revoke("phone");
        assert!(registry.deliveries(101).is_empty());
    }
    #[test]
    fn the_activity_ends_only_when_the_authoritative_display_is_empty() {
        let mut registry = Registry::default();
        registry
            .register("phone", &params("activity", 1), "PC", display(1, 1, 0), 100)
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
        registry.update(display(0, 1, 0), 101);
        let remaining = registry.deliveries(101).remove(0);
        assert_eq!(aps(&remaining)["event"], "update");
        assert_eq!(
            aps(&remaining)["content-state"]["display"]["current"]["label"],
            "確認待ち 1件"
        );
        registry.complete(&remaining, ResultKind::Accepted, 101);
        registry.update(display(0, 0, 0), 102);
        registry.update(display(1, 0, 0), 103);
        registry
            .register("phone", &params("activity", 2), "PC", display(1, 0, 0), 103)
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
    fn an_unchanged_unknown_display_does_not_end_or_flood_the_activity() {
        let mut registry = Registry::default();
        registry
            .register("phone", &params("activity", 1), "PC", display(1, 0, 0), 100)
            .unwrap();
        let first = registry.deliveries(100).remove(0);
        assert!(first.urgent);
        assert!(aps(&first).get("stale-date").is_none());
        registry.complete(&first, ResultKind::Accepted, 100);
        assert!(registry.deliveries(159).is_empty());
        let heartbeat = registry.deliveries(160).remove(0);
        assert!(!heartbeat.urgent);
        registry.complete(&heartbeat, ResultKind::Accepted, 160);
        registry.update(display(0, 0, 1), 161);
        let uncertain = registry.deliveries(161).remove(0);
        assert_eq!(aps(&uncertain)["event"], "update");
        assert_eq!(
            aps(&uncertain)["content-state"]["display"]["current"]["label"],
            "状態確認中 1件"
        );
        assert!(aps(&uncertain).get("stale-date").is_none());
        registry.complete(&uncertain, ResultKind::Accepted, 161);
        registry.update(display(0, 0, 1), 162);
        assert!(registry.deliveries(162).is_empty());
        registry.update(display(0, 1, 0), 164);
        let waiting = registry.deliveries(164).remove(0);
        assert_eq!(aps(&waiting)["relevance-score"], 100);
        registry.complete(&waiting, ResultKind::Accepted, 164);
        assert!(!registry.deliveries(224).remove(0).urgent);
    }
    #[test]
    fn a_task_change_before_the_first_delivery_cannot_lower_registration_priority() {
        let mut registry = Registry::default();
        registry
            .register("phone", &params("activity", 1), "PC", display(1, 0, 0), 100)
            .unwrap();
        registry.update(display(2, 0, 0), 100);
        let initial = registry.deliveries(100).remove(0);
        assert!(initial.urgent);
        registry.complete(&initial, ResultKind::Accepted, 100);
        registry.update(display(1, 0, 0), 101);
        let completion = registry.deliveries(101).remove(0);
        assert!(completion.urgent);
        registry.complete(&completion, ResultKind::Accepted, 101);
        assert!(!registry.deliveries(161).remove(0).urgent);
    }
    #[test]
    fn registration_capacity_expiry_and_payload_size_are_bounded() {
        let mut registry = Registry::default();
        for i in 0..16 {
            registry
                .register(
                    "phone",
                    &params(&i.to_string(), 1),
                    "PC",
                    display(1, 0, 0),
                    100,
                )
                .unwrap();
        }
        assert!(
            registry
                .register("phone", &params("overflow", 1), "PC", display(1, 0, 0), 100)
                .is_err()
        );
        for i in 1..16 {
            for activity in 0..16 {
                registry
                    .register(
                        &format!("phone{i}"),
                        &params(&activity.to_string(), 1),
                        "PC",
                        display(1, 0, 0),
                        100,
                    )
                    .unwrap();
            }
        }
        assert!(
            registry
                .register("last", &params("overflow", 1), "PC", display(1, 0, 0), 100)
                .is_err()
        );
        registry
            .register(
                "phone",
                &params("0", 2),
                "PC",
                display(1, 0, 0),
                100 + LIFETIME - 1,
            )
            .unwrap();
        assert!(registry.deliveries(100 + LIFETIME).is_empty());
        registry
            .register(
                "phone",
                &params("new", 1),
                "PC",
                display(1, 0, 0),
                100 + LIFETIME,
            )
            .unwrap();
        let delivery = registry.deliveries(100 + LIFETIME).remove(0);
        registry.complete(&delivery, ResultKind::Expired, 100 + LIFETIME);
        assert!(registry.entries.is_empty());
    }
    #[test]
    fn push_start_retries_latest_state_and_dismissal_survives_restart_until_idle() {
        let mut registry = Registry {
            host_id: "host".into(),
            ..Default::default()
        };
        let mut start = params("unused", 1);
        start.activity_id = None;
        start.allow_start = true;
        registry
            .register("phone", &start, "PC", display(1, 0, 0), 100)
            .unwrap();
        let delivery = registry.deliveries(100).remove(0);
        assert_eq!(aps(&delivery)["event"], "start");
        assert_eq!(aps(&delivery)["attributes"]["hostID"], "host");
        assert_eq!(aps(&delivery)["input-push-token"], 1);
        registry.complete(&delivery, ResultKind::Retry, 100);
        assert!(registry.deliveries(109).is_empty());
        registry.update(display(0, 1, 0), 110);
        let delivery = registry.deliveries(110).remove(0);
        assert_eq!(
            aps(&delivery)["content-state"]["display"]["current"]["label"],
            "確認待ち 1件"
        );
        registry.complete(&delivery, ResultKind::Accepted, 110);
        registry
            .register("phone", &params("activity", 2), "PC", display(0, 1, 0), 111)
            .unwrap();
        registry.unregister("phone", "activity");
        let bytes = Zeroizing::new(serde_json::to_vec(&registry).unwrap());
        let mut restarted: Registry = serde_json::from_slice(&bytes).unwrap();
        restarted.update(display(2, 0, 0), 200);
        assert!(restarted.deliveries(200).is_empty());
        start.token = vec![3; 32];
        restarted
            .register("phone", &start, "PC", display(2, 0, 0), 201)
            .unwrap();
        assert!(
            restarted.deliveries(201).is_empty(),
            "rotation cannot override dismissal"
        );
        restarted.update(display(0, 0, 1), 202);
        assert!(
            restarted.deliveries(202).is_empty(),
            "unknown state cannot reset dismissal"
        );
        restarted.update(display(0, 0, 0), 203);
        assert!(
            restarted.deliveries(203).is_empty(),
            "start tokens never receive end events"
        );
        restarted.update(display(1, 0, 0), 204);
        assert_eq!(aps(&restarted.deliveries(204).remove(0))["event"], "start");
    }
    #[test]
    fn start_requires_permission_and_an_existing_activity_suppresses_duplicates() {
        let mut registry = Registry::default();
        let mut start = params("unused", 1);
        start.activity_id = None;
        registry
            .register("phone", &start, "PC", display(1, 0, 0), 100)
            .unwrap();
        assert!(registry.deliveries(100).is_empty());
        start.allow_start = true;
        registry
            .register("phone", &start, "PC", display(1, 0, 0), 101)
            .unwrap();
        registry
            .register("phone", &params("activity", 2), "PC", display(1, 0, 0), 101)
            .unwrap();
        assert!(
            registry
                .deliveries(101)
                .iter()
                .all(|delivery| delivery.activity.is_some())
        );
        registry.revoke("phone");
        assert!(registry.deliveries(200).is_empty());
    }
    proptest::proptest! {
        #[test]
        fn counts_fit_push_payload_even_with_many_tasks(count in 1u32..10000) {
            let mut registry = Registry::default();
            registry.register("phone", &params("activity", 1), "PC", display(count, 0, 0), 100).unwrap();
            let delivery = registry.deliveries(100).remove(0);
            proptest::prop_assert!(delivery.payload.len() < 4096);
            proptest::prop_assert_eq!(aps(&delivery)["content-state"]["display"]["current"]["total"].as_u64(), Some(u64::from(count)));
        }
    }
}
