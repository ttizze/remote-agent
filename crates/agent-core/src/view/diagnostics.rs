//! Pure rows for the Host background and local diagnostics surfaces.
//! Native clients consume these rows instead of reimplementing policy labels.
use agent_protocol::background::{
    BackgroundPolicySnapshot, HostResourcesSnapshot, ProcessDiagnosticsResult,
    ProcessResourceHistoryResult, TraceDiagnosticsResult,
};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiagnosticRow {
    pub key: String,
    pub value: String,
}

fn profile_id(profile: agent_domain::BackgroundActivityProfile) -> &'static str {
    match profile {
        agent_domain::BackgroundActivityProfile::Balanced => "balanced",
        agent_domain::BackgroundActivityProfile::Performance => "performance",
        agent_domain::BackgroundActivityProfile::BatterySaver => "battery-saver",
    }
}

pub fn background_rows(snapshot: &BackgroundPolicySnapshot) -> Vec<DiagnosticRow> {
    vec![
        DiagnosticRow { key: "profile".into(), value: profile_id(snapshot.policy.profile).into() },
        DiagnosticRow {
            key: "automaticGitFetchIntervalMs".into(),
            value: snapshot.policy.automatic_git_fetch_interval_ms.to_string(),
        },
        DiagnosticRow {
            key: "providerHealthRefreshIntervalMs".into(),
            value: snapshot.policy.provider_health_refresh_interval_ms.to_string(),
        },
        DiagnosticRow {
            key: "hostPowerMonitorActiveIntervalMs".into(),
            value: snapshot.policy.host_power_monitor_active_interval_ms.to_string(),
        },
        DiagnosticRow {
            key: "hostPowerMonitorIdleIntervalMs".into(),
            value: snapshot.policy.host_power_monitor_idle_interval_ms.to_string(),
        },
        DiagnosticRow {
            key: "idleClientTtlMs".into(),
            value: snapshot.policy.idle_client_ttl_ms.to_string(),
        },
        DiagnosticRow {
            key: "pauseWhenHostLocked".into(),
            value: snapshot.policy.pause_when_host_locked.to_string(),
        },
        DiagnosticRow {
            key: "pauseWhenHostLowPower".into(),
            value: snapshot.policy.pause_when_host_low_power.to_string(),
        },
        DiagnosticRow {
            key: "pauseWhenClientLowPower".into(),
            value: snapshot.policy.pause_when_client_low_power.to_string(),
        },
        DiagnosticRow {
            key: "pauseWhenOnBattery".into(),
            value: snapshot.policy.pause_when_on_battery.to_string(),
        },
        DiagnosticRow { key: "activeLeases".into(), value: snapshot.leases.len().to_string() },
        DiagnosticRow { key: "activeScopes".into(), value: snapshot.active_scope_keys.len().to_string() },
        DiagnosticRow { key: "foregroundLeases".into(), value: snapshot.active_foreground_lease_count.to_string() },
        DiagnosticRow { key: "opportunisticWork".into(), value: snapshot.should_run_opportunistic_work.to_string() },
        DiagnosticRow { key: "hostPower".into(), value: format!("{:?}", snapshot.host_power.source) },
        DiagnosticRow {
            key: "hostPowerSpeedLimitPercent".into(),
            value: snapshot
                .host_power
                .speed_limit_percent
                .map_or_else(|| "unknown".into(), |value| format!("{value}%")),
        },
    ]
}

pub fn host_resource_rows(snapshot: &HostResourcesSnapshot) -> Vec<DiagnosticRow> {
    if !snapshot.usable_for_load_balancing() {
        return vec![DiagnosticRow {
            key: "status".into(),
            value: "unavailable".into(),
        }];
    }
    vec![
        DiagnosticRow {
            key: "cpuUtilization".into(),
            value: snapshot.cpu_utilization.map_or_else(
                || "unknown".into(),
                |value| format!("{:.1}%", value * 100.0),
            ),
        },
        DiagnosticRow {
            key: "memory".into(),
            value: format!("{}/{} bytes", snapshot.available_memory_bytes, snapshot.total_memory_bytes),
        },
        DiagnosticRow { key: "cpuCount".into(), value: snapshot.cpu_count.to_string() },
    ]
}

pub fn process_rows(result: &ProcessDiagnosticsResult) -> Vec<DiagnosticRow> {
    result.processes.iter().map(|entry| DiagnosticRow {
        key: format!("{}:{}", entry.pid, entry.start_time_ms),
        value: format!("{} · {}", entry.elapsed, entry.command),
    }).collect()
}

pub fn process_history_rows(result: &ProcessResourceHistoryResult) -> Vec<DiagnosticRow> {
    result.top_processes.iter().map(|entry| DiagnosticRow {
        key: entry.process_key.clone(),
        value: format!("{:.1}% · {} samples", entry.current_cpu_percent, entry.sample_count),
    }).collect()
}

pub fn trace_rows(result: &TraceDiagnosticsResult) -> Vec<DiagnosticRow> {
    vec![
        DiagnosticRow { key: "records".into(), value: result.record_count.to_string() },
        DiagnosticRow { key: "failures".into(), value: result.failure_count.to_string() },
        DiagnosticRow { key: "interruptions".into(), value: result.interruption_count.to_string() },
        DiagnosticRow { key: "slowSpans".into(), value: result.slow_span_count.to_string() },
        DiagnosticRow { key: "parseErrors".into(), value: result.parse_error_count.to_string() },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{BackgroundActivityPolicy, BackgroundActivityProfile, HostPowerSnapshot, Timestamp};

    #[test]
    fn background_rows_keep_policy_projection_stable() {
        let at = Timestamp::from_millis(1).unwrap();
        let snapshot = BackgroundPolicySnapshot {
            policy: BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced),
            host_power: HostPowerSnapshot::unknown(at.clone()),
            leases: vec![],
            active_foreground_lease_count: 0,
            active_scope_keys: vec![],
            should_run_opportunistic_work: false,
            updated_at: at,
        };
        assert_eq!(background_rows(&snapshot)[0].value, "balanced");
    }

    #[test]
    fn host_resource_rows_wait_for_a_valid_capacity_probe() {
        let rows = host_resource_rows(&HostResourcesSnapshot {
            sampled_at: 1,
            cpu_utilization: None,
            cpu_count: 0,
            available_memory_bytes: 0,
            total_memory_bytes: 0,
        });
        assert_eq!(rows, vec![DiagnosticRow { key: "status".into(), value: "unavailable".into() }]);
    }
}
