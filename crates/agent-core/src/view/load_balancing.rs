//! Pure capacity rules for device-local, cross-environment load balancing.
use agent_protocol::background::HostResourcesSnapshot;

/// The four preferences exposed by native settings. A missing value uses the
/// normal 50% preference, so a newly registered environment never receives a
/// surprising share of automatic drafts.
pub const DEFAULT_WEIGHT: u8 = 50;
pub const PREFERENCE_WEIGHTS: [u8; 4] = [100, 50, 25, 0];

/// Resolves a saved weight to one of the preferences exposed by the settings
/// UI. Values below or above Normal snap to the adjacent choice so every
/// client renders one of the four fixed labels.
pub fn preference_for_weight(weight: Option<u8>) -> u8 {
    match weight {
        None | Some(DEFAULT_WEIGHT) => DEFAULT_WEIGHT,
        Some(0) => 0,
        Some(value) if value < DEFAULT_WEIGHT => 25,
        Some(_) => 100,
    }
}

/// One connected Host that has the same logical project and selected
/// provider as the draft being opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub environment_id: String,
    pub resources: Option<HostResourcesSnapshot>,
    /// The time this client received the resource reply. Host sample clocks
    /// are deliberately not used for freshness decisions.
    pub received_at_ms: Option<i64>,
    pub weight: u8,
}

/// Selects the machine with the best weighted free capacity.
///
/// This mirrors the web runtime rule: stale, future-dated, incomplete or
/// saturated samples are ignored; ties retain the caller's deterministic
/// candidate order. The domain-owned `usable_for_load_balancing` predicate is
/// the first capacity gate so clients do not duplicate probe validation.
pub fn select_environment<'a>(
    candidates: &'a [Candidate],
    now_ms: i64,
) -> Option<&'a str> {
    let mut selected = None;
    let mut best_score = 0.0_f64;
    for candidate in candidates {
        let Some(resources) = candidate.resources.as_ref() else {
            continue;
        };
        let Some(received_at_ms) = candidate.received_at_ms else {
            continue;
        };
        if candidate.weight == 0 || !resources.usable_for_load_balancing() {
            continue;
        }
        let age_ms = now_ms.saturating_sub(received_at_ms);
        if age_ms > 15_000 || age_ms < -5_000 {
            continue;
        }
        let Some(cpu_utilization) = resources.cpu_utilization else {
            continue;
        };
        if cpu_utilization >= 0.95 {
            continue;
        }
        let available_memory = resources.available_memory_bytes as f64
            / resources.total_memory_bytes as f64;
        if available_memory <= 0.05 {
            continue;
        }
        let score = f64::from(candidate.weight)
            * resources.cpu_count as f64
            * (1.0 - cpu_utilization)
            * available_memory;
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
    use proptest::prelude::*;

    fn resources(now_ms: i64) -> HostResourcesSnapshot {
        HostResourcesSnapshot {
            sampled_at: now_ms as u64,
            cpu_utilization: Some(0.2),
            cpu_count: 8,
            available_memory_bytes: 8_000,
            total_memory_bytes: 16_000,
        }
    }

    fn candidate(id: &str, resources: Option<HostResourcesSnapshot>, weight: u8) -> Candidate {
        Candidate {
            environment_id: id.into(),
            resources,
            received_at_ms: Some(100_000),
            weight,
        }
    }

    #[test]
    fn compares_capacity_and_preference() {
        let now = 100_000;
        let mut busy = resources(now);
        busy.cpu_utilization = Some(0.9);
        let mut idle = resources(now);
        idle.cpu_count = 4;
        let preferred = resources(now);
        assert_eq!(
            select_environment(
                &[
                    candidate("busy", Some(busy), 100),
                    candidate("idle", Some(idle), 100),
                    candidate("preferred", Some(preferred), 100),
                ],
                now,
            ),
            Some("preferred")
        );
        assert_eq!(
            select_environment(
                &[
                    candidate("idle", Some(resources(now)), 50),
                    candidate("preferred", Some(resources(now)), 100),
                ],
                now,
            ),
            Some("preferred")
        );
    }

    #[test]
    fn rejects_stale_unknown_and_saturated_samples() {
        let now = 100_000;
        let mut stale = candidate("stale", Some(resources(now)), 100);
        stale.received_at_ms = Some(now - 30_000);
        let mut no_cpu = resources(now);
        no_cpu.cpu_utilization = None;
        let mut full_cpu = resources(now);
        full_cpu.cpu_utilization = Some(0.95);
        let mut full_memory = resources(now);
        full_memory.available_memory_bytes = 100;
        let candidates = vec![
            stale,
            candidate("unknown", None, 100),
            candidate("no-cpu", Some(no_cpu), 100),
            candidate("disabled", Some(resources(now)), 0),
            candidate("full-cpu", Some(full_cpu), 100),
            candidate("full-memory", Some(full_memory), 100),
        ];
        assert_eq!(select_environment(&candidates, now), None);
    }

    #[test]
    fn uses_client_receipt_time_and_never_remote_sample_clock() {
        let now = 100_000;
        let mut remote_clock = candidate("remote-clock", Some(resources(now + 60_000)), 100);
        remote_clock.received_at_ms = Some(now);
        assert_eq!(
            select_environment(&[remote_clock.clone()], now),
            Some("remote-clock")
        );
        assert_eq!(select_environment(&[remote_clock], now + 15_001), None);
        let mut missing_receipt = candidate("missing-receipt", Some(resources(now)), 100);
        missing_receipt.received_at_ms = None;
        assert_eq!(select_environment(&[missing_receipt], now), None);
    }

    #[test]
    fn normalizes_preferences_to_the_four_saved_choices() {
        assert_eq!(preference_for_weight(None), 50);
        assert_eq!(preference_for_weight(Some(0)), 0);
        assert_eq!(preference_for_weight(Some(10)), 25);
        assert_eq!(preference_for_weight(Some(50)), 50);
        assert_eq!(preference_for_weight(Some(80)), 100);
        assert_eq!(PREFERENCE_WEIGHTS, [100, 50, 25, 0]);
    }

    proptest! {
        #[test]
        fn a_fresh_usable_machine_is_selectable(
            cpu_utilization in 0.0_f64..0.95,
            cpu_count in 1_u64..=64,
            total_memory_bytes in 2_u64..=1_000_000,
            weight in 1_u8..=100,
        ) {
            let resources = HostResourcesSnapshot {
                sampled_at: 100_000,
                cpu_utilization: Some(cpu_utilization),
                cpu_count,
                available_memory_bytes: total_memory_bytes / 2,
                total_memory_bytes,
            };
            let candidate = Candidate {
                environment_id: "fresh".into(),
                resources: Some(resources),
                received_at_ms: Some(100_000),
                weight,
            };
            prop_assert_eq!(select_environment(&[candidate], 100_000), Some("fresh"));
        }
    }
}
