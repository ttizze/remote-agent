//! Pure routing rules for device-local provider-instance load balancing.
use agent_domain::Driver;
use agent_protocol::models::HostResourcesSnapshot;
use std::collections::BTreeMap;

/// A machine with no saved preference participates at the reference Normal
/// weight. The four values are the only preferences shown by native settings.
pub const DEFAULT_WEIGHT: u8 = 50;
pub const PREFERENCE_WEIGHTS: [u8; 4] = [100, 50, 25, 0];

/// Snaps values written by older or non-native clients to a visible load
/// preference while preserving the explicit manual-only value.
pub fn preference_weight(weight: Option<u8>) -> u8 {
    match weight {
        None | Some(50) => DEFAULT_WEIGHT,
        Some(0) => 0,
        Some(weight) if weight < 50 => 25,
        Some(_) => 100,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub instance_id: String,
    pub driver: Driver,
    pub ready: bool,
}

/// Selects one ready instance using the saved integer weights. The seed is
/// supplied by the owner so the same new-thread draft remains stable while it
/// is being edited; no mutable router state is needed.
pub fn select_instance<'a>(
    candidates: &'a [Candidate],
    driver: Driver,
    weights: &BTreeMap<String, u8>,
    seed: u64,
) -> Option<&'a str> {
    let eligible: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.driver == driver && candidate.ready)
        .filter_map(|candidate| {
            let weight = preference_weight(weights.get(&candidate.instance_id).copied());
            (weight > 0).then_some((candidate, u64::from(weight)))
        })
        .collect();
    let total = eligible.iter().map(|(_, weight)| *weight).sum::<u64>();
    if total == 0 {
        return None;
    }
    let mut slot = seed % total;
    for (candidate, weight) in eligible {
        if slot < weight {
            return Some(candidate.instance_id.as_str());
        }
        slot -= weight;
    }
    None
}

/// A stable, process-independent seed for a new-thread route.
pub fn seed(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

/// A connected-environment candidate with a Host resource receipt. The
/// timestamp is supplied by the receiving client so clocks on different
/// environments are never compared directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceCandidate {
    pub environment_id: String,
    pub resources: Option<HostResourcesSnapshot>,
    pub received_at: Option<u64>,
    pub weight: f64,
}

/// Chooses the least busy environment using the reference resource rules.
/// Samples older than 15 seconds, more than five seconds in the future, or
/// with unsafe capacity readings are excluded before scoring.
pub fn select_resource_balanced(candidates: &[ResourceCandidate], now_ms: u64) -> Option<&str> {
    let mut selected = None;
    let mut best_score = 0.0_f64;
    for candidate in candidates {
        let Some(resources) = candidate.resources else {
            continue;
        };
        let sampled_at = candidate.received_at.unwrap_or(resources.sampled_at);
        if !candidate.weight.is_finite()
            || candidate.weight <= 0.0
            || now_ms.saturating_sub(sampled_at) > 15_000
            || sampled_at > now_ms.saturating_add(5_000)
            || resources.cpu_utilization.is_none_or(|cpu| cpu >= 0.95)
            || resources.cpu_count == 0
            || resources.total_memory_bytes == 0
        {
            continue;
        }
        let available_fraction =
            resources.available_memory_bytes as f64 / resources.total_memory_bytes as f64;
        if available_fraction <= 0.05 {
            continue;
        }
        let score = candidate.weight
            * resources.cpu_count as f64
            * (1.0 - resources.cpu_utilization.unwrap_or(1.0))
            * available_fraction;
        if score > best_score {
            selected = Some(candidate.environment_id.as_str());
            best_score = score;
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, driver: Driver) -> Candidate {
        Candidate {
            instance_id: id.into(),
            driver,
            ready: true,
        }
    }

    #[test]
    fn disabled_and_wrong_driver_instances_are_not_routed() {
        let mut offline = candidate("offline", Driver::Codex);
        offline.ready = false;
        let candidates = vec![offline, candidate("claude", Driver::Claude)];
        assert_eq!(
            select_instance(&candidates, Driver::Codex, &BTreeMap::new(), 0),
            None
        );
    }

    #[test]
    fn weights_choose_a_stable_ready_instance_and_zero_disables_one() {
        let candidates = vec![
            candidate("one", Driver::Codex),
            candidate("two", Driver::Codex),
        ];
        let weights = BTreeMap::from([(String::from("one"), 0), (String::from("two"), 25)]);
        assert_eq!(
            select_instance(&candidates, Driver::Codex, &weights, 0),
            Some("two")
        );
        assert_eq!(
            select_instance(&candidates, Driver::Codex, &weights, 100),
            Some("two")
        );
        assert_eq!(
            select_instance(&candidates, Driver::Codex, &weights, 1),
            Some("two")
        );
    }

    #[test]
    fn missing_and_saved_weights_use_the_four_reference_preferences() {
        assert_eq!(preference_weight(None), 50);
        assert_eq!(preference_weight(Some(50)), 50);
        assert_eq!(preference_weight(Some(100)), 100);
        assert_eq!(preference_weight(Some(25)), 25);
        assert_eq!(preference_weight(Some(1)), 25);
        assert_eq!(preference_weight(Some(0)), 0);
        assert_eq!(PREFERENCE_WEIGHTS, [100, 50, 25, 0]);
    }

    #[test]
    fn resource_balancing_rejects_stale_or_overloaded_hosts() {
        let snapshot =
            |sampled_at, cpu_utilization, available_memory_bytes| HostResourcesSnapshot {
                sampled_at,
                cpu_utilization,
                cpu_count: 8,
                available_memory_bytes,
                total_memory_bytes: 100,
            };
        let candidates = vec![
            ResourceCandidate {
                environment_id: "stale".into(),
                resources: Some(snapshot(1, Some(0.01), 90)),
                received_at: None,
                weight: f64::from(DEFAULT_WEIGHT),
            },
            ResourceCandidate {
                environment_id: "busy".into(),
                resources: Some(snapshot(10_000, Some(0.99), 90)),
                received_at: None,
                weight: 100.0,
            },
            ResourceCandidate {
                environment_id: "ready".into(),
                resources: Some(snapshot(10_000, Some(0.2), 80)),
                received_at: Some(10_000),
                weight: 100.0,
            },
        ];
        assert_eq!(select_resource_balanced(&candidates, 10_000), Some("ready"));
        assert_eq!(select_resource_balanced(&candidates, 30_001), None);
    }
}
