//! Pure routing rules for device-local provider-instance load balancing.
use agent_domain::Driver;
use std::collections::BTreeMap;

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
            let weight = weights.get(&candidate.instance_id).copied().unwrap_or(100);
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
}
