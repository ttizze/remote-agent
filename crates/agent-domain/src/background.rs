//! Pure background policy, host power, and resource diagnostics contracts.
//!
//! The module owns decisions and projections only.  Clocks, process probes,
//! power notifications, file reads, and persistence stay with their owners.
use crate::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_CLIENT_ACTIVITY_TTL_MS: u64 = 45_000;
pub const MAX_CLIENT_ACTIVITY_TTL_MS: u64 = 120_000;
pub const MIN_CLIENT_ACTIVITY_TTL_MS: u64 = 1_000;
pub const MAX_CLIENT_ACTIVITY_LEASES_PER_RPC_CLIENT: usize = 16;
pub const RESOURCE_HISTORY_MAX_WINDOW_MS: u64 = 60 * 60_000;
pub const RESOURCE_HISTORY_MAX_TOP_PROCESSES: usize = 100;
pub const TRACE_TOP_LIMIT: usize = 10;
pub const TRACE_RECENT_LIMIT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundBooleanState {
    True,
    False,
    #[default]
    Unknown,
}
impl BackgroundBooleanState {
    pub fn is_true(self) -> bool {
        matches!(self, Self::True)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HostPowerThermalState {
    #[default]
    Unknown,
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HostPowerSource {
    #[default]
    Unknown,
    NodeMacosShell,
    NodeMacosNative,
    NodeLinux,
    NodeWindows,
    DesktopMain,
    ElectronMain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPowerSnapshot {
    pub source: HostPowerSource,
    pub idle: BackgroundBooleanState,
    pub idle_seconds: Option<u64>,
    pub locked: BackgroundBooleanState,
    /// A false value is an observed awake state only when `stale` is false;
    /// stale snapshots do not claim that suspend was observed.
    pub suspended: bool,
    pub on_battery: BackgroundBooleanState,
    pub low_power_mode: BackgroundBooleanState,
    pub thermal_state: HostPowerThermalState,
    /// The desktop power publisher supplies the OS thermal speed limit when
    /// it exposes one.  A missing value is different from a 100% limit.
    pub speed_limit_percent: Option<u8>,
    /// Processes sampled by the supervised local desktop.  The Host keeps
    /// this alongside its own process tree because the desktop is a sibling
    /// process, not a Host child.
    pub desktop_processes: Vec<ResourceProcess>,
    pub stale: bool,
    pub updated_at: Timestamp,
}
impl HostPowerSnapshot {
    pub fn unknown(updated_at: Timestamp) -> Self {
        Self {
            source: HostPowerSource::Unknown,
            idle: BackgroundBooleanState::Unknown,
            idle_seconds: None,
            locked: BackgroundBooleanState::Unknown,
            suspended: false,
            on_battery: BackgroundBooleanState::Unknown,
            low_power_mode: BackgroundBooleanState::Unknown,
            thermal_state: HostPowerThermalState::Unknown,
            speed_limit_percent: None,
            desktop_processes: Vec::new(),
            stale: true,
            updated_at,
        }
    }

    /// Idle time and timestamps are heartbeats.  They do not constitute a
    /// semantic power change and therefore must not wake background workers.
    pub fn same_state(&self, other: &Self) -> bool {
        self.source == other.source
            && self.idle == other.idle
            && self.locked == other.locked
            && self.suspended == other.suspended
            && self.on_battery == other.on_battery
            && self.low_power_mode == other.low_power_mode
            && self.thermal_state == other.thermal_state
            && self.speed_limit_percent == other.speed_limit_percent
            && self.stale == other.stale
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundActivityProfile {
    #[default]
    Balanced,
    Performance,
    BatterySaver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivityPolicy {
    pub profile: BackgroundActivityProfile,
    pub automatic_git_fetch_interval_ms: u64,
    pub provider_health_refresh_interval_ms: u64,
    pub host_power_monitor_active_interval_ms: u64,
    pub host_power_monitor_idle_interval_ms: u64,
    pub idle_client_ttl_ms: u64,
    pub pause_when_host_locked: bool,
    pub pause_when_host_low_power: bool,
    pub pause_when_client_low_power: bool,
    pub pause_when_on_battery: bool,
}
impl BackgroundActivityPolicy {
    pub fn preset(profile: BackgroundActivityProfile) -> Self {
        match profile {
            BackgroundActivityProfile::Performance => Self {
                profile,
                automatic_git_fetch_interval_ms: 15_000,
                provider_health_refresh_interval_ms: 60_000,
                host_power_monitor_active_interval_ms: 30_000,
                host_power_monitor_idle_interval_ms: 120_000,
                idle_client_ttl_ms: DEFAULT_CLIENT_ACTIVITY_TTL_MS,
                pause_when_host_locked: true,
                pause_when_host_low_power: false,
                pause_when_client_low_power: false,
                pause_when_on_battery: false,
            },
            BackgroundActivityProfile::Balanced => Self {
                profile,
                automatic_git_fetch_interval_ms: 30_000,
                provider_health_refresh_interval_ms: 300_000,
                host_power_monitor_active_interval_ms: 30_000,
                host_power_monitor_idle_interval_ms: 300_000,
                idle_client_ttl_ms: DEFAULT_CLIENT_ACTIVITY_TTL_MS,
                pause_when_host_locked: true,
                pause_when_host_low_power: true,
                pause_when_client_low_power: true,
                pause_when_on_battery: false,
            },
            BackgroundActivityProfile::BatterySaver => Self {
                profile,
                automatic_git_fetch_interval_ms: 0,
                provider_health_refresh_interval_ms: 900_000,
                host_power_monitor_active_interval_ms: 60_000,
                host_power_monitor_idle_interval_ms: 600_000,
                idle_client_ttl_ms: DEFAULT_CLIENT_ACTIVITY_TTL_MS,
                pause_when_host_locked: true,
                pause_when_host_low_power: true,
                pause_when_client_low_power: true,
                pause_when_on_battery: true,
            },
        }
    }

    pub fn normalized(mut self) -> Self {
        self.idle_client_ttl_ms = self
            .idle_client_ttl_ms
            .clamp(MIN_CLIENT_ACTIVITY_TTL_MS, MAX_CLIENT_ACTIVITY_TTL_MS);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum BackgroundScope {
    ServerConfig,
    ProviderStatus {
        #[serde(rename = "instanceId", skip_serializing_if = "Option::is_none")]
        instance_id: Option<String>,
    },
    VcsStatus {
        cwd: String,
    },
    GitRefs {
        cwd: String,
    },
    Diagnostics,
    Thread {
        #[serde(rename = "threadId")]
        thread_id: String,
    },
}

pub fn scope_key(scope: &BackgroundScope) -> String {
    match scope {
        BackgroundScope::ServerConfig => "server-config".into(),
        BackgroundScope::ProviderStatus { instance_id } => instance_id
            .as_deref()
            .map(|id| format!("provider-status:{id}"))
            .unwrap_or_else(|| "provider-status".into()),
        BackgroundScope::VcsStatus { cwd } => format!("vcs-status:{cwd}"),
        BackgroundScope::GitRefs { cwd } => format!("git-refs:{cwd}"),
        BackgroundScope::Diagnostics => "diagnostics".into(),
        BackgroundScope::Thread { thread_id } => format!("thread:{thread_id}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundClientKind {
    Web,
    DesktopRenderer,
    Mobile,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundAppState {
    Active,
    Inactive,
    Background,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundBatteryState {
    #[default]
    Unknown,
    Unplugged,
    Charging,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientActivityReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    pub client_id: String,
    pub client_kind: BackgroundClientKind,
    pub visible: bool,
    pub focused: bool,
    pub recently_interacted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_state: Option<BackgroundAppState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_power_mode: Option<BackgroundBooleanState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_state: Option<BackgroundBatteryState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_type: Option<String>,
    pub scopes: Vec<BackgroundScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    pub observed_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientActivityLease {
    pub client_id: String,
    pub client_kind: BackgroundClientKind,
    pub visible: bool,
    pub focused: bool,
    pub recently_interacted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_state: Option<BackgroundAppState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_power_mode: Option<BackgroundBooleanState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_state: Option<BackgroundBatteryState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_type: Option<String>,
    pub scopes: Vec<BackgroundScope>,
    pub session_id: String,
    pub rpc_client_id: u64,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
}

impl ClientActivityReport {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.client_id.trim().is_empty() {
            return Err("client id must not be empty");
        }
        if self.client_id.trim() != self.client_id || self.client_id.chars().count() > 128 {
            return Err("client id must be trimmed and at most 128 characters");
        }
        Ok(())
    }
    pub fn ttl_ms(&self, policy: &BackgroundActivityPolicy) -> u64 {
        self.ttl_ms
            .unwrap_or(policy.idle_client_ttl_ms)
            .clamp(MIN_CLIENT_ACTIVITY_TTL_MS, MAX_CLIENT_ACTIVITY_TTL_MS)
    }
    pub fn lease_at(
        &self,
        session_id: impl Into<String>,
        rpc_client_id: u64,
        policy: &BackgroundActivityPolicy,
        updated_at: &Timestamp,
    ) -> Result<ClientActivityLease, &'static str> {
        self.validate()?;
        let ttl = self.ttl_ms(policy) as i64;
        let expires_at = Timestamp::from_millis(updated_at.millis().saturating_add(ttl))
            .map_err(|_| "activity lease expiry is outside timestamp range")?;
        Ok(ClientActivityLease {
            client_id: self.client_id.clone(),
            client_kind: self.client_kind,
            visible: self.visible,
            focused: self.focused,
            recently_interacted: self.recently_interacted,
            app_state: self.app_state.clone(),
            low_power_mode: self.low_power_mode,
            battery_state: self.battery_state,
            network_type: self.network_type.clone(),
            scopes: self.scopes.clone(),
            session_id: session_id.into(),
            rpc_client_id,
            updated_at: updated_at.clone(),
            expires_at,
        })
    }
}

pub fn lease_key(session_id: &str, rpc_client_id: u64, client_id: &str) -> String {
    format!("{session_id}\u{0}{rpc_client_id}\u{0}{client_id}")
}

fn lease_active(lease: &ClientActivityLease, now_ms: i64) -> bool {
    lease.expires_at.millis() > now_ms
}

pub fn upsert_client_activity_lease(
    leases: &BTreeMap<String, ClientActivityLease>,
    lease: ClientActivityLease,
    now: &Timestamp,
) -> BTreeMap<String, ClientActivityLease> {
    let now_ms = now.millis();
    let mut next: BTreeMap<_, _> = leases
        .iter()
        .filter(|(_, value)| lease_active(value, now_ms))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let key = lease_key(&lease.session_id, lease.rpc_client_id, &lease.client_id);
    if !next.contains_key(&key) {
        let same_connection = next
            .iter()
            .filter(|(_, value)| {
                value.session_id == lease.session_id && value.rpc_client_id == lease.rpc_client_id
            })
            .map(|(key, value)| (key.clone(), value.updated_at.millis()))
            .collect::<Vec<_>>();
        if same_connection.len() >= MAX_CLIENT_ACTIVITY_LEASES_PER_RPC_CLIENT
            && let Some((oldest, _)) = same_connection
                .into_iter()
                .min_by_key(|(key, updated)| (*updated, key.clone()))
        {
            next.remove(&oldest);
        }
    }
    next.insert(key, lease);
    next
}

pub fn remove_rpc_client(
    leases: &BTreeMap<String, ClientActivityLease>,
    session_id: &str,
    rpc_client_id: u64,
) -> BTreeMap<String, ClientActivityLease> {
    leases
        .iter()
        .filter(|(_, lease)| lease.session_id != session_id || lease.rpc_client_id != rpc_client_id)
        .map(|(key, lease)| (key.clone(), lease.clone()))
        .collect()
}

pub fn host_power_constrained(
    power: &HostPowerSnapshot,
    policy: &BackgroundActivityPolicy,
) -> bool {
    if power.stale {
        return false;
    }
    power.suspended
        || (policy.pause_when_host_locked && power.locked.is_true())
        || matches!(
            power.thermal_state,
            HostPowerThermalState::Serious | HostPowerThermalState::Critical
        )
        || (policy.pause_when_host_low_power && power.low_power_mode.is_true())
        || (policy.pause_when_on_battery && power.on_battery.is_true())
}

pub fn client_power_constrained(
    lease: &ClientActivityLease,
    policy: &BackgroundActivityPolicy,
) -> bool {
    (policy.pause_when_client_low_power
        && lease.low_power_mode == Some(BackgroundBooleanState::True))
        || (policy.pause_when_on_battery
            && lease.battery_state == Some(BackgroundBatteryState::Unplugged))
}

pub fn lease_foreground(lease: &ClientActivityLease, now: &Timestamp) -> bool {
    lease_active(lease, now.millis())
        && lease.visible
        && (lease.focused || lease.recently_interacted)
}

pub fn lease_may_run_scoped_work(
    lease: &ClientActivityLease,
    scope: &BackgroundScope,
    now: &Timestamp,
    policy: &BackgroundActivityPolicy,
) -> bool {
    lease_active(lease, now.millis())
        && lease
            .scopes
            .iter()
            .any(|candidate| scope_key(candidate) == scope_key(scope))
        && !client_power_constrained(lease, policy)
        && (matches!(policy.profile, BackgroundActivityProfile::Performance)
            || lease_foreground(lease, now))
}

pub fn compute_background_snapshot(
    host_power: HostPowerSnapshot,
    leases: &BTreeMap<String, ClientActivityLease>,
    now: &Timestamp,
    policy: &BackgroundActivityPolicy,
) -> BackgroundPolicySnapshot {
    let active: Vec<_> = leases
        .values()
        .filter(|lease| lease_active(lease, now.millis()))
        .cloned()
        .collect();
    let active_foreground_lease_count = active
        .iter()
        .filter(|lease| lease_foreground(lease, now))
        .count();
    let active_scope_keys = active
        .iter()
        .flat_map(|lease| lease.scopes.iter().map(scope_key))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let constrained = host_power_constrained(&host_power, policy);
    let should_run_opportunistic_work = !constrained
        && active
            .iter()
            .any(|lease| lease_foreground(lease, now) && !client_power_constrained(lease, policy));
    BackgroundPolicySnapshot {
        policy: policy.clone(),
        host_power,
        leases: active,
        active_foreground_lease_count,
        active_scope_keys,
        should_run_opportunistic_work,
        updated_at: now.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundPolicySnapshot {
    /// The normalized settings currently used by the Host's background owners.
    /// Keeping this beside the derived demand state lets native settings and
    /// diagnostics render the same values that gate work.
    pub policy: BackgroundActivityPolicy,
    pub host_power: HostPowerSnapshot,
    pub leases: Vec<ClientActivityLease>,
    pub active_foreground_lease_count: usize,
    pub active_scope_keys: Vec<String>,
    pub should_run_opportunistic_work: bool,
    pub updated_at: Timestamp,
}

pub fn background_work_due(
    last_run: Option<&Timestamp>,
    now: &Timestamp,
    interval_ms: u64,
) -> bool {
    if interval_ms == 0 {
        return false;
    }
    match last_run {
        None => true,
        Some(last) => {
            now.millis() < last.millis()
                || now.millis().saturating_sub(last.millis()) >= interval_ms as i64
        }
    }
}

// Resource telemetry -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceProcessCategory {
    Server,
    ServerChild,
    ProviderRoot,
    TerminalRoot,
    ElectronMain,
    ElectronRenderer,
    ElectronGpu,
    ElectronUtility,
    ResourceMonitor,
    #[serde(rename = "unknown-application")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceTelemetryIoSemantics {
    Storage,
    Logical,
    AllIo,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceProcessIdentity {
    pub pid: u32,
    pub start_time_ms: u64,
}
impl ResourceProcessIdentity {
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.start_time_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceProcessSample {
    pub identity: ResourceProcessIdentity,
    pub ppid: u32,
    pub run_time_ms: u64,
    pub name: String,
    pub command: String,
    pub status: String,
    pub category: ResourceProcessCategory,
    pub cpu_percent: f64,
    pub cpu_time_ms: u64,
    pub resident_bytes: u64,
    pub virtual_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub io_semantics: ResourceTelemetryIoSemantics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceProcess {
    pub identity: ResourceProcessIdentity,
    pub ppid: u32,
    pub child_pids: Vec<u32>,
    pub depth: u32,
    pub name: String,
    pub command: String,
    pub status: String,
    pub category: ResourceProcessCategory,
    pub cpu_percent: f64,
    pub cpu_time_ms: u64,
    pub resident_bytes: u64,
    pub peak_resident_bytes: u64,
    pub virtual_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub io_read_bytes_per_second: f64,
    pub io_write_bytes_per_second: f64,
    pub io_semantics: ResourceTelemetryIoSemantics,
    pub run_time_ms: u64,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAggregate {
    pub process_count: u64,
    pub current_cpu_percent: f64,
    pub cpu_time_ms: u64,
    pub current_rss_bytes: u64,
    pub peak_rss_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub io_read_bytes_per_second: f64,
    pub io_write_bytes_per_second: f64,
    pub process_starts: u64,
    pub process_exits: u64,
}
impl Default for ResourceAggregate {
    fn default() -> Self {
        Self {
            process_count: 0,
            current_cpu_percent: 0.0,
            cpu_time_ms: 0,
            current_rss_bytes: 0,
            peak_rss_bytes: 0,
            io_read_bytes: 0,
            io_write_bytes: 0,
            io_read_bytes_per_second: 0.0,
            io_write_bytes_per_second: 0.0,
            process_starts: 0,
            process_exits: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResourcesSnapshot {
    pub sampled_at: u64,
    pub cpu_utilization: Option<f64>,
    pub cpu_count: u64,
    pub available_memory_bytes: u64,
    pub total_memory_bytes: u64,
}
impl HostResourcesSnapshot {
    /// A zero capacity means the native probe failed.  Callers that use host
    /// capacity for scheduling must wait for this predicate instead of
    /// interpreting an unavailable probe as an empty machine.
    pub fn usable_for_load_balancing(&self) -> bool {
        self.cpu_count > 0
            && self.total_memory_bytes > 0
            && self.available_memory_bytes <= self.total_memory_bytes
            && self
                .cpu_utilization
                .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceSourceStatus {
    #[default]
    Starting,
    Healthy,
    Degraded,
    Unavailable,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSourceHealth {
    pub status: ResourceSourceStatus,
    pub last_sample_at: Option<Timestamp>,
    pub last_error: Option<String>,
}
impl Default for ResourceSourceHealth {
    fn default() -> Self {
        Self {
            status: ResourceSourceStatus::Starting,
            last_sample_at: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceHealth {
    pub native: ResourceSourceHealth,
    pub desktop: ResourceSourceHealth,
    pub sidecar_version: Option<String>,
    pub sidecar_pid: Option<u32>,
    pub restart_count: u64,
    pub collection_duration_micros: u64,
    pub scanned_process_count: u64,
    pub retained_process_count: u64,
    pub inaccessible_process_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAttributionEntry {
    pub component: String,
    pub operation: String,
    pub logical_read_bytes: u64,
    pub logical_write_bytes: u64,
    pub count: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceAttributionSnapshot {
    pub read_at: Timestamp,
    pub entries: Vec<ResourceAttributionEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTelemetrySnapshot {
    pub read_at: Timestamp,
    pub sample_interval_ms: u64,
    pub processes: Vec<ResourceProcess>,
    pub groups: ResourceGroups,
    pub power: HostPowerSnapshot,
    pub speed_limit_percent: Option<f64>,
    pub attribution: ResourceAttributionSnapshot,
    pub health: ResourceHealth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceGroups {
    pub backend: ResourceAggregate,
    pub electron: ResourceAggregate,
    pub monitor: ResourceAggregate,
    pub all_processes: ResourceAggregate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceHistoryBucket {
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub max_rss_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub max_process_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceProcessSummary {
    pub identity: ResourceProcessIdentity,
    pub ppid: u32,
    pub depth: u32,
    pub name: String,
    pub command: String,
    pub category: ResourceProcessCategory,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub current_cpu_percent: f64,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub cpu_time_ms: u64,
    pub current_rss_bytes: u64,
    pub peak_rss_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub io_semantics: ResourceTelemetryIoSemantics,
    pub sample_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTelemetryHistory {
    pub read_at: Timestamp,
    pub window_ms: u64,
    pub bucket_ms: u64,
    pub sample_interval_ms: u64,
    pub retained_sample_count: u64,
    pub buckets: Vec<ResourceHistoryBucket>,
    pub top_processes: Vec<ResourceProcessSummary>,
    pub health: ResourceHealth,
}

pub fn normalize_resource_history_window(window_ms: u64, bucket_ms: u64) -> (u64, u64) {
    let window_ms = window_ms.clamp(1_000, RESOURCE_HISTORY_MAX_WINDOW_MS);
    (window_ms, bucket_ms.clamp(1_000, window_ms))
}

fn signalable_category(category: ResourceProcessCategory) -> bool {
    matches!(
        category,
        ResourceProcessCategory::ServerChild
            | ResourceProcessCategory::ProviderRoot
            | ResourceProcessCategory::TerminalRoot
            | ResourceProcessCategory::ElectronMain
            | ResourceProcessCategory::ElectronRenderer
            | ResourceProcessCategory::ElectronGpu
            | ResourceProcessCategory::ElectronUtility
            | ResourceProcessCategory::Unknown
    )
}

fn format_elapsed(run_time_ms: u64) -> String {
    let total_seconds = run_time_ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessDiagnosticsEntry {
    pub pid: u32,
    pub start_time_ms: u64,
    pub ppid: u32,
    pub pgid: Option<u32>,
    pub status: String,
    pub cpu_percent: f64,
    pub rss_bytes: u64,
    pub elapsed: String,
    pub command: String,
    pub depth: u32,
    pub child_pids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessDiagnosticsError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessDiagnosticsResult {
    pub server_pid: u32,
    pub read_at: Timestamp,
    pub process_count: u64,
    pub total_rss_bytes: u64,
    pub total_cpu_percent: f64,
    pub processes: Vec<ProcessDiagnosticsEntry>,
    pub error: Option<ProcessDiagnosticsError>,
}

pub fn process_diagnostics(
    server_pid: u32,
    read_at: Timestamp,
    processes: &[ResourceProcess],
    error: Option<ProcessDiagnosticsError>,
) -> ProcessDiagnosticsResult {
    let processes: Vec<_> = processes
        .iter()
        .filter(|entry| signalable_category(entry.category))
        .map(|entry| ProcessDiagnosticsEntry {
            pid: entry.identity.pid,
            start_time_ms: entry.identity.start_time_ms,
            ppid: entry.ppid,
            pgid: None,
            status: if entry.status.is_empty() {
                "Unknown".into()
            } else {
                entry.status.clone()
            },
            cpu_percent: entry.cpu_percent,
            rss_bytes: entry.resident_bytes,
            elapsed: format_elapsed(entry.run_time_ms),
            command: if !entry.command.is_empty() {
                entry.command.clone()
            } else if !entry.name.is_empty() {
                entry.name.clone()
            } else {
                "unknown".into()
            },
            depth: entry.depth.saturating_sub(1),
            child_pids: entry.child_pids.clone(),
        })
        .collect();
    ProcessDiagnosticsResult {
        server_pid,
        read_at,
        process_count: processes.len() as u64,
        total_rss_bytes: processes.iter().map(|entry| entry.rss_bytes).sum(),
        total_cpu_percent: processes.iter().map(|entry| entry.cpu_percent).sum(),
        processes,
        error,
    }
}

pub fn project_process_resource_history(
    history: &ResourceTelemetryHistory,
) -> ProcessResourceHistoryResult {
    let top_processes: Vec<_> = history
        .top_processes
        .iter()
        .filter(|entry| {
            matches!(
                entry.category,
                ResourceProcessCategory::Server
                    | ResourceProcessCategory::ServerChild
                    | ResourceProcessCategory::ProviderRoot
                    | ResourceProcessCategory::TerminalRoot
                    | ResourceProcessCategory::ElectronMain
                    | ResourceProcessCategory::ElectronRenderer
                    | ResourceProcessCategory::ElectronGpu
                    | ResourceProcessCategory::ElectronUtility
                    | ResourceProcessCategory::Unknown
            )
        })
        .map(|entry| ProcessResourceEntry {
            process_key: entry.identity.key(),
            pid: entry.identity.pid,
            ppid: entry.ppid,
            command: if entry.command.is_empty() {
                if entry.name.is_empty() {
                    "unknown".into()
                } else {
                    entry.name.clone()
                }
            } else {
                entry.command.clone()
            },
            depth: entry.depth,
            is_server_root: entry.category == ResourceProcessCategory::Server,
            first_seen_at: entry.first_seen_at.clone(),
            last_seen_at: entry.last_seen_at.clone(),
            current_cpu_percent: entry.current_cpu_percent,
            avg_cpu_percent: entry.avg_cpu_percent,
            max_cpu_percent: entry.max_cpu_percent,
            cpu_seconds_approx: entry.cpu_time_ms as f64 / 1_000.0,
            current_rss_bytes: entry.current_rss_bytes,
            max_rss_bytes: entry.peak_rss_bytes,
            sample_count: entry.sample_count,
        })
        .collect();
    let total_cpu_seconds_approx = top_processes
        .iter()
        .map(|entry| entry.cpu_seconds_approx)
        .sum();
    ProcessResourceHistoryResult {
        read_at: history.read_at.clone(),
        window_ms: history.window_ms,
        bucket_ms: history.bucket_ms,
        sample_interval_ms: history.sample_interval_ms,
        retained_sample_count: history.retained_sample_count,
        total_cpu_seconds_approx,
        buckets: history
            .buckets
            .iter()
            .map(|bucket| ProcessResourceHistoryBucket {
                started_at: bucket.started_at.clone(),
                ended_at: bucket.ended_at.clone(),
                avg_cpu_percent: bucket.avg_cpu_percent,
                max_cpu_percent: bucket.max_cpu_percent,
                max_rss_bytes: bucket.max_rss_bytes,
                max_process_count: bucket.max_process_count,
            })
            .collect(),
        top_processes,
        error: history.health.native.last_error.clone().map(|message| {
            ProcessResourceHistoryError {
                failure_tag: "ProcessDiagnosticsQueryFailedError".into(),
                message,
            }
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResourceEntry {
    pub process_key: String,
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
    pub depth: u32,
    pub is_server_root: bool,
    pub first_seen_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub current_cpu_percent: f64,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub cpu_seconds_approx: f64,
    pub current_rss_bytes: u64,
    pub max_rss_bytes: u64,
    pub sample_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResourceHistoryBucket {
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub max_rss_bytes: u64,
    pub max_process_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResourceHistoryError {
    pub failure_tag: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResourceHistoryResult {
    pub read_at: Timestamp,
    pub window_ms: u64,
    pub bucket_ms: u64,
    pub sample_interval_ms: u64,
    pub retained_sample_count: u64,
    pub total_cpu_seconds_approx: f64,
    pub buckets: Vec<ProcessResourceHistoryBucket>,
    pub top_processes: Vec<ProcessResourceEntry>,
    pub error: Option<ProcessResourceHistoryError>,
}

// Trace diagnostics --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceDiagnosticsErrorKind {
    TraceFileNotFound,
    TraceFileReadFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceDiagnosticsError {
    pub kind: TraceDiagnosticsErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSpanSummary {
    pub name: String,
    pub count: u64,
    pub failure_count: u64,
    pub total_duration_ms: f64,
    pub average_duration_ms: f64,
    pub max_duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSpanOccurrence {
    pub name: String,
    pub duration_ms: f64,
    pub ended_at: Timestamp,
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceFailureSummary {
    pub name: String,
    pub cause: String,
    pub count: u64,
    pub last_seen_at: Timestamp,
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecentFailure {
    pub name: String,
    pub duration_ms: f64,
    pub ended_at: Timestamp,
    pub trace_id: String,
    pub span_id: String,
    pub cause: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLogEvent {
    pub span_name: String,
    pub level: String,
    pub message: String,
    pub seen_at: Timestamp,
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceDiagnosticsResult {
    pub trace_file_path: String,
    pub scanned_file_paths: Vec<String>,
    pub read_at: Timestamp,
    pub record_count: u64,
    pub parse_error_count: u64,
    pub first_span_at: Option<Timestamp>,
    pub last_span_at: Option<Timestamp>,
    pub failure_count: u64,
    pub interruption_count: u64,
    pub slow_span_threshold_ms: f64,
    pub slow_span_count: u64,
    pub log_level_counts: BTreeMap<String, u64>,
    pub top_spans_by_count: Vec<TraceSpanSummary>,
    pub slowest_spans: Vec<TraceSpanOccurrence>,
    pub common_failures: Vec<TraceFailureSummary>,
    pub latest_failures: Vec<TraceRecentFailure>,
    pub latest_warning_and_error_logs: Vec<TraceLogEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_failure: Option<bool>,
    pub error: Option<TraceDiagnosticsError>,
}

fn json_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty())
}
fn json_number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}
fn nanos_timestamp(value: Option<&Value>) -> Option<Timestamp> {
    let text = json_string(value)?;
    let millis = text.parse::<u128>().ok()?.checked_div(1_000_000)?;
    Timestamp::from_millis(i64::try_from(millis).ok()?).ok()
}

fn insert_slowest(items: &mut Vec<TraceSpanOccurrence>, item: TraceSpanOccurrence) {
    items.push(item);
    items.sort_by(|left, right| {
        right
            .duration_ms
            .total_cmp(&left.duration_ms)
            .then_with(|| left.span_id.cmp(&right.span_id))
    });
    items.truncate(TRACE_TOP_LIMIT);
}
fn insert_latest<T, F>(items: &mut Vec<T>, item: T, compare: F)
where
    F: Fn(&T, &T) -> std::cmp::Ordering,
{
    items.push(item);
    items.sort_by(compare);
    items.truncate(TRACE_RECENT_LIMIT);
}

#[derive(Debug, Clone)]
pub struct TraceDiagnosticsAggregator {
    slow_span_threshold_ms: f64,
    parse_error_count: u64,
    record_count: u64,
    failure_count: u64,
    interruption_count: u64,
    slow_span_count: u64,
    first_span_at: Option<Timestamp>,
    last_span_at: Option<Timestamp>,
    spans_by_name: BTreeMap<String, TraceSpanSummary>,
    failures_by_key: BTreeMap<String, TraceFailureSummary>,
    latest_failures: Vec<TraceRecentFailure>,
    slowest_spans: Vec<TraceSpanOccurrence>,
    latest_warning_and_error_logs: Vec<TraceLogEvent>,
    log_level_counts: BTreeMap<String, u64>,
}
impl TraceDiagnosticsAggregator {
    pub fn new(slow_span_threshold_ms: f64) -> Self {
        Self {
            slow_span_threshold_ms,
            parse_error_count: 0,
            record_count: 0,
            failure_count: 0,
            interruption_count: 0,
            slow_span_count: 0,
            first_span_at: None,
            last_span_at: None,
            spans_by_name: BTreeMap::new(),
            failures_by_key: BTreeMap::new(),
            latest_failures: Vec::new(),
            slowest_spans: Vec::new(),
            latest_warning_and_error_logs: Vec::new(),
            log_level_counts: BTreeMap::new(),
        }
    }

    pub fn add_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            self.parse_error_count += 1;
            return;
        };
        let Some(object) = value.as_object() else {
            self.parse_error_count += 1;
            return;
        };
        let Some(name) = json_string(object.get("name")) else {
            self.parse_error_count += 1;
            return;
        };
        let Some(trace_id) = json_string(object.get("traceId")) else {
            self.parse_error_count += 1;
            return;
        };
        let Some(span_id) = json_string(object.get("spanId")) else {
            self.parse_error_count += 1;
            return;
        };
        let Some(duration_ms) = json_number(object.get("durationMs")) else {
            self.parse_error_count += 1;
            return;
        };
        let Some(ended_at) = nanos_timestamp(object.get("endTimeUnixNano")) else {
            self.parse_error_count += 1;
            return;
        };
        let started_at = nanos_timestamp(object.get("startTimeUnixNano"));
        self.record_count += 1;
        if started_at.as_ref().is_some_and(|started_at| {
            self.first_span_at
                .as_ref()
                .is_none_or(|old| started_at < old)
        }) {
            self.first_span_at = started_at;
        }
        if self.last_span_at.as_ref().is_none_or(|old| &ended_at > old) {
            self.last_span_at = Some(ended_at.clone());
        }
        let exit = object.get("exit").and_then(Value::as_object);
        let exit_tag = exit.and_then(|value| json_string(value.get("_tag")));
        let is_failure = exit_tag.as_deref() == Some("Failure");
        let is_interrupted = exit_tag.as_deref() == Some("Interrupted");
        self.failure_count += u64::from(is_failure);
        self.interruption_count += u64::from(is_interrupted);
        if duration_ms >= self.slow_span_threshold_ms {
            self.slow_span_count += 1;
        }
        let summary = self
            .spans_by_name
            .entry(name.clone())
            .or_insert(TraceSpanSummary {
                name: name.clone(),
                count: 0,
                failure_count: 0,
                total_duration_ms: 0.0,
                average_duration_ms: 0.0,
                max_duration_ms: 0.0,
            });
        summary.count += 1;
        summary.failure_count += u64::from(is_failure);
        summary.total_duration_ms += duration_ms;
        summary.average_duration_ms = summary.total_duration_ms / summary.count as f64;
        summary.max_duration_ms = summary.max_duration_ms.max(duration_ms);
        let occurrence = TraceSpanOccurrence {
            name: name.clone(),
            duration_ms,
            ended_at: ended_at.clone(),
            trace_id: trace_id.clone(),
            span_id: span_id.clone(),
        };
        insert_slowest(&mut self.slowest_spans, occurrence.clone());
        if is_failure {
            let cause = exit
                .and_then(|value| json_string(value.get("cause")))
                .unwrap_or_else(|| "Failure".into());
            insert_latest(
                &mut self.latest_failures,
                TraceRecentFailure {
                    name: name.clone(),
                    duration_ms,
                    ended_at: ended_at.clone(),
                    trace_id: trace_id.clone(),
                    span_id: span_id.clone(),
                    cause: cause.clone(),
                },
                |left, right| right.ended_at.cmp(&left.ended_at),
            );
            let key = format!("{name}\u{0}{cause}");
            let existing = self
                .failures_by_key
                .entry(key)
                .or_insert(TraceFailureSummary {
                    name: name.clone(),
                    cause: cause.clone(),
                    count: 0,
                    last_seen_at: ended_at.clone(),
                    trace_id: trace_id.clone(),
                    span_id: span_id.clone(),
                });
            existing.count += 1;
            if ended_at > existing.last_seen_at {
                existing.last_seen_at = ended_at.clone();
                existing.trace_id = trace_id.clone();
                existing.span_id = span_id.clone();
            }
        }
        if let Some(events) = object.get("events").and_then(Value::as_array) {
            for event in events {
                let Some(event) = event.as_object() else {
                    continue;
                };
                let Some(attributes) = event.get("attributes").and_then(Value::as_object) else {
                    continue;
                };
                let Some(level) = json_string(attributes.get("effect.logLevel")) else {
                    continue;
                };
                *self.log_level_counts.entry(level.clone()).or_default() += 1;
                if !matches!(
                    level.to_ascii_lowercase().as_str(),
                    "warning" | "warn" | "error" | "fatal"
                ) {
                    continue;
                }
                let seen_at =
                    nanos_timestamp(event.get("timeUnixNano")).unwrap_or_else(|| ended_at.clone());
                let message = json_string(event.get("name")).unwrap_or_else(|| "Log event".into());
                insert_latest(
                    &mut self.latest_warning_and_error_logs,
                    TraceLogEvent {
                        span_name: name.clone(),
                        level,
                        message,
                        seen_at,
                        trace_id: trace_id.clone(),
                        span_id: span_id.clone(),
                    },
                    |left, right| right.seen_at.cmp(&left.seen_at),
                );
            }
        }
    }

    pub fn finish(
        self,
        trace_file_path: impl Into<String>,
        scanned_file_paths: Vec<String>,
        read_at: Timestamp,
        error: Option<TraceDiagnosticsError>,
        partial_failure: Option<bool>,
    ) -> TraceDiagnosticsResult {
        let mut top_spans_by_count: Vec<_> = self.spans_by_name.into_values().collect();
        top_spans_by_count.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| right.max_duration_ms.total_cmp(&left.max_duration_ms))
        });
        top_spans_by_count.truncate(TRACE_TOP_LIMIT);
        let mut common_failures: Vec<_> = self.failures_by_key.into_values().collect();
        common_failures.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| right.last_seen_at.cmp(&left.last_seen_at))
        });
        common_failures.truncate(TRACE_TOP_LIMIT);
        TraceDiagnosticsResult {
            trace_file_path: trace_file_path.into(),
            scanned_file_paths,
            read_at,
            record_count: self.record_count,
            parse_error_count: self.parse_error_count,
            first_span_at: self.first_span_at,
            last_span_at: self.last_span_at,
            failure_count: self.failure_count,
            interruption_count: self.interruption_count,
            slow_span_threshold_ms: self.slow_span_threshold_ms,
            slow_span_count: self.slow_span_count,
            log_level_counts: self.log_level_counts,
            top_spans_by_count,
            slowest_spans: self.slowest_spans,
            common_failures,
            latest_failures: self.latest_failures,
            latest_warning_and_error_logs: self.latest_warning_and_error_logs,
            partial_failure,
            error,
        }
    }
}

pub fn empty_trace_diagnostics(
    trace_file_path: impl Into<String>,
    scanned_file_paths: Vec<String>,
    read_at: Timestamp,
    slow_span_threshold_ms: f64,
    error: TraceDiagnosticsError,
) -> TraceDiagnosticsResult {
    TraceDiagnosticsAggregator::new(slow_span_threshold_ms).finish(
        trace_file_path,
        scanned_file_paths,
        read_at,
        Some(error),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms).unwrap()
    }
    fn report(overrides: impl FnOnce(&mut ClientActivityReport)) -> ClientActivityReport {
        let mut value = ClientActivityReport {
            environment_id: None,
            client_id: "client-1".into(),
            client_kind: BackgroundClientKind::Web,
            visible: true,
            focused: true,
            recently_interacted: true,
            app_state: Some(BackgroundAppState::Active),
            low_power_mode: Some(BackgroundBooleanState::Unknown),
            battery_state: Some(BackgroundBatteryState::Unknown),
            network_type: None,
            scopes: vec![BackgroundScope::VcsStatus {
                cwd: "/repo".into(),
            }],
            ttl_ms: Some(45_000),
            observed_at: at(1_000),
        };
        overrides(&mut value);
        value
    }

    #[test]
    fn policy_presets_match_background_activity_defaults() {
        assert_eq!(
            BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced)
                .automatic_git_fetch_interval_ms,
            30_000
        );
        assert_eq!(
            BackgroundActivityPolicy::preset(BackgroundActivityProfile::Performance)
                .provider_health_refresh_interval_ms,
            60_000
        );
        assert_eq!(
            BackgroundActivityPolicy::preset(BackgroundActivityProfile::BatterySaver)
                .automatic_git_fetch_interval_ms,
            0
        );
    }

    #[test]
    fn stale_host_values_do_not_gate_work() {
        let policy = BackgroundActivityPolicy::preset(BackgroundActivityProfile::BatterySaver);
        let power = HostPowerSnapshot {
            locked: BackgroundBooleanState::True,
            on_battery: BackgroundBooleanState::True,
            low_power_mode: BackgroundBooleanState::True,
            thermal_state: HostPowerThermalState::Critical,
            ..HostPowerSnapshot::unknown(at(1_000))
        };
        assert!(!host_power_constrained(&power, &policy));
    }

    #[test]
    fn lease_limit_evicts_oldest_and_preserves_new_client() {
        let policy = BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced);
        let mut leases = BTreeMap::new();
        for index in 0..=MAX_CLIENT_ACTIVITY_LEASES_PER_RPC_CLIENT {
            let mut value = report(|report| {
                report.client_id = format!("client-{index}");
            });
            value.ttl_ms = Some(60_000);
            let lease = value
                .lease_at("session", 1, &policy, &at(index as i64))
                .unwrap();
            leases = upsert_client_activity_lease(&leases, lease, &at(index as i64));
        }
        assert_eq!(leases.len(), MAX_CLIENT_ACTIVITY_LEASES_PER_RPC_CLIENT);
        assert!(leases.values().any(|lease| lease.client_id == "client-16"));
        assert!(!leases.values().any(|lease| lease.client_id == "client-0"));
    }

    #[test]
    fn scoped_work_requires_matching_foreground_demand_except_performance() {
        let balanced = BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced);
        let lease = report(|report| {
            report.focused = false;
            report.visible = false;
        })
        .lease_at("session", 1, &balanced, &at(1_000))
        .unwrap();
        let scope = BackgroundScope::VcsStatus {
            cwd: "/repo".into(),
        };
        assert!(!lease_may_run_scoped_work(
            &lease,
            &scope,
            &at(2_000),
            &balanced
        ));
        let performance = BackgroundActivityPolicy::preset(BackgroundActivityProfile::Performance);
        assert!(lease_may_run_scoped_work(
            &lease,
            &scope,
            &at(2_000),
            &performance
        ));
    }

    #[test]
    fn semantic_power_changes_ignore_idle_heartbeat() {
        let initial = HostPowerSnapshot {
            stale: false,
            ..HostPowerSnapshot::unknown(at(1_000))
        };
        let heartbeat = HostPowerSnapshot {
            idle_seconds: Some(10),
            updated_at: at(2_000),
            ..initial.clone()
        };
        let changed = HostPowerSnapshot {
            locked: BackgroundBooleanState::True,
            updated_at: at(3_000),
            ..initial.clone()
        };
        assert!(initial.same_state(&heartbeat));
        assert!(!initial.same_state(&changed));
    }

    #[test]
    fn resource_wire_keys_use_application_names() {
        let groups = ResourceGroups {
            backend: ResourceAggregate::default(),
            electron: ResourceAggregate::default(),
            monitor: ResourceAggregate::default(),
            all_processes: ResourceAggregate::default(),
        };
        let encoded = serde_json::to_value(&groups).unwrap();
        assert!(encoded.get("allProcesses").is_some());
        assert_eq!(
            serde_json::to_value(ResourceProcessCategory::Unknown).unwrap(),
            serde_json::json!("unknown-application")
        );
    }

    #[test]
    fn interval_due_handles_first_run_disabled_and_clock_rollback() {
        assert!(background_work_due(None, &at(10_000), 1_000));
        assert!(!background_work_due(Some(&at(10_000)), &at(10_999), 1_000));
        assert!(background_work_due(Some(&at(10_000)), &at(9_999), 1_000));
        assert!(!background_work_due(Some(&at(10_000)), &at(11_000), 0));
    }

    #[test]
    fn same_client_update_replaces_without_consuming_a_lease_slot() {
        let policy = BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced);
        let first = report(|_| {})
            .lease_at("session", 1, &policy, &at(1_000))
            .unwrap();
        let second = report(|report| {
            report.focused = false;
        })
        .lease_at("session", 1, &policy, &at(2_000))
        .unwrap();
        let leases = upsert_client_activity_lease(&BTreeMap::new(), first, &at(1_000));
        let leases = upsert_client_activity_lease(&leases, second, &at(2_000));
        assert_eq!(leases.len(), 1);
        assert!(!leases.values().next().unwrap().focused);
    }

    #[test]
    fn trace_aggregator_keeps_bounded_failure_and_slow_lists() {
        let mut aggregator = TraceDiagnosticsAggregator::new(10.0);
        for index in 0..30 {
            aggregator.add_line(&serde_json::json!({
                "name": "span", "traceId": format!("trace-{index}"), "spanId": format!("span-{index}"),
                "startTimeUnixNano": ((index * 1_000) as u64 * 1_000_000).to_string(),
                "endTimeUnixNano": ((index * 1_000 + index) as u64 * 1_000_000).to_string(),
                "durationMs": index, "exit": {"_tag":"Failure", "cause":"failed"},
            }).to_string());
        }
        let result = aggregator.finish("trace", vec!["trace".into()], at(100_000), None, None);
        assert_eq!(result.record_count, 30);
        assert_eq!(result.slowest_spans.len(), TRACE_TOP_LIMIT);
        assert_eq!(result.latest_failures.len(), TRACE_RECENT_LIMIT);
        assert_eq!(result.common_failures[0].count, 30);
    }

    #[test]
    fn trace_aggregator_rejects_records_without_required_identity_or_end_time() {
        let mut aggregator = TraceDiagnosticsAggregator::new(1.0);
        aggregator.add_line(
            &serde_json::json!({
                "name": "missing-end",
                "traceId": "trace",
                "spanId": "span",
                "durationMs": 2.0,
            })
            .to_string(),
        );
        let result = aggregator.finish("trace", vec!["trace".into()], at(100_000), None, None);
        assert_eq!(result.record_count, 0);
        assert_eq!(result.parse_error_count, 1);
    }

    proptest! {
        #[test]
        fn lease_snapshot_never_reports_expired_leases(ttl in 1u64..120_001, now in 0i64..100_000) {
            let policy = BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced);
            let mut value = report(|report| { report.ttl_ms = Some(ttl); });
            value.ttl_ms = Some(ttl);
            let lease = value.lease_at("session", 1, &policy, &at(now)).unwrap();
            let expiry = now
                + ttl.clamp(MIN_CLIENT_ACTIVITY_TTL_MS, MAX_CLIENT_ACTIVITY_TTL_MS) as i64;
            let leases = upsert_client_activity_lease(&BTreeMap::new(), lease, &at(expiry));
            let snapshot = compute_background_snapshot(HostPowerSnapshot::unknown(at(expiry)), &leases, &at(expiry), &policy);
            prop_assert!(snapshot.leases.is_empty());
        }
    }
}
