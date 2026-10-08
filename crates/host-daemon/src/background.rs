//! Host-owned background policy, power probes, local resource samples, and
//! trace diagnostics.  The domain crate supplies all decisions and views.
use crate::power_events::SuspendLifecycleSource;
use agent_domain::{
    BackgroundActivityPolicy, BackgroundBooleanState, BackgroundPolicySnapshot, BackgroundScope,
    ClientActivityLease, HostPowerSnapshot, HostPowerSource, ResourceAggregate,
    ResourceAttributionSnapshot, ResourceHealth, ResourceHistoryBucket, ResourceProcess,
    ResourceProcessCategory, ResourceProcessIdentity, ResourceProcessSummary, ResourceSourceHealth,
    ResourceSourceStatus, ResourceTelemetryHistory, ResourceTelemetryIoSemantics,
    ResourceTelemetrySnapshot, Timestamp, TraceDiagnosticsAggregator, TraceDiagnosticsError,
    TraceDiagnosticsErrorKind, TraceDiagnosticsResult, compute_background_snapshot,
    host_power_constrained, lease_may_run_scoped_work, normalize_resource_history_window,
    process_diagnostics, project_process_resource_history, remove_rpc_client,
    upsert_client_activity_lease,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt};
use tokio::sync::{Mutex as TokioMutex, RwLock, broadcast};
use tokio_util::sync::CancellationToken;

const RESOURCE_HISTORY_LIMIT: usize = 720;
const RESOURCE_HISTORY_MAX_AGE_MS: i64 = 60 * 60_000;
const RESOURCE_HISTORY_MAX_BYTES: usize = 8 * 1024 * 1024;
const RESOURCE_HISTORY_MAX_ENTRIES: usize = 20_000;
const RESOURCE_PROCESS_LIMIT: usize = 256;
const RESOURCE_CHILD_PID_LIMIT: usize = 256;
const RESOURCE_TEXT_LIMIT: usize = 1_024;
const RESOURCE_ATTRIBUTION_LIMIT: usize = 256;
const TRACE_MAX_FILES: u32 = 16;
const DESKTOP_POWER_HEALTH_TIMEOUT_MS: i64 = 10 * 60_000;

fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Timestamp::from_millis(millis).expect("system time is within timestamp range")
}

fn unknown_power(at: Timestamp) -> HostPowerSnapshot {
    HostPowerSnapshot::unknown(at)
}

fn native_power_source() -> HostPowerSource {
    #[cfg(target_os = "linux")]
    {
        HostPowerSource::NodeLinux
    }
    #[cfg(target_os = "macos")]
    {
        HostPowerSource::NodeMacosNative
    }
    #[cfg(target_os = "windows")]
    {
        HostPowerSource::NodeWindows
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        HostPowerSource::Unknown
    }
}

/// Semantic host-power state.  Idle seconds are retained in the snapshot but
/// do not cause a policy publication by themselves.
pub(crate) struct HostPowerMonitor {
    state: RwLock<HostPowerMonitorState>,
}

struct HostPowerMonitorState {
    latest: HostPowerSnapshot,
    /// A suspend/resume notification is authoritative until the matching
    /// notification arrives.  Native samples may continue while a machine is
    /// waking, but must not turn a real suspend notification into an awake
    /// state before the OS reports resume.
    lifecycle_suspended: Option<bool>,
}
impl HostPowerMonitor {
    fn new() -> Self {
        Self {
            state: RwLock::new(HostPowerMonitorState {
                latest: unknown_power(now()),
                lifecycle_suspended: None,
            }),
        }
    }

    async fn snapshot(&self) -> HostPowerSnapshot {
        self.state.read().await.latest.clone()
    }

    async fn report(&self, mut next: HostPowerSnapshot) -> bool {
        let mut state = self.state.write().await;
        if next.updated_at < state.latest.updated_at {
            return false;
        }
        if let Some(suspended) = state.lifecycle_suspended {
            next.suspended = suspended;
        }
        if state.latest.same_state(&next) {
            if next.updated_at > state.latest.updated_at {
                state.latest = next;
            }
            return false;
        }
        state.latest = next;
        true
    }

    async fn report_lifecycle(&self, suspended: bool) -> bool {
        self.report_lifecycle_at(suspended, now()).await
    }

    async fn report_lifecycle_at(&self, suspended: bool, updated_at: Timestamp) -> bool {
        let mut state = self.state.write().await;
        if updated_at < state.latest.updated_at {
            return false;
        }
        state.lifecycle_suspended = Some(suspended);
        let mut next = state.latest.clone();
        if matches!(
            next.source,
            HostPowerSource::Unknown | HostPowerSource::DesktopMain | HostPowerSource::ElectronMain
        ) {
            next.source = native_power_source();
        }
        next.suspended = suspended;
        next.stale = false;
        next.updated_at = updated_at;
        if state.latest.same_state(&next) {
            state.latest = next;
            return false;
        }
        state.latest = next;
        true
    }
}

struct ResourceState {
    snapshots: VecDeque<ResourceTelemetrySnapshot>,
    system: System,
    host_cpu_refreshed_at: Option<Instant>,
    process_cpu_time_ms: BTreeMap<String, u64>,
    host_resources: Option<agent_domain::HostResourcesSnapshot>,
    host_resources_refreshed_at: Option<Instant>,
    desktop_processes: Vec<ResourceProcess>,
    desktop_processes_sampled_at: Option<Timestamp>,
    attribution: BTreeMap<(String, String), agent_domain::ResourceAttributionEntry>,
    health: ResourceHealth,
    lifecycle: ResourceLifecycleCounters,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResourceLifecycleGroup {
    cpu_time_ms: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
    process_starts: u64,
    process_exits: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResourceLifecycleCounters {
    backend: ResourceLifecycleGroup,
    electron: ResourceLifecycleGroup,
    monitor: ResourceLifecycleGroup,
    all_processes: ResourceLifecycleGroup,
}

/// Resource sampling is demand driven by diagnostics calls.  This keeps an
/// idle Host from doing process scans while preserving a bounded local history.
pub(crate) struct ResourceOwner {
    state: Mutex<ResourceState>,
}
impl ResourceOwner {
    fn new() -> Self {
        Self {
            state: Mutex::new(ResourceState {
                snapshots: VecDeque::new(),
                system: System::new(),
                host_cpu_refreshed_at: None,
                process_cpu_time_ms: BTreeMap::new(),
                host_resources: None,
                host_resources_refreshed_at: None,
                desktop_processes: Vec::new(),
                desktop_processes_sampled_at: None,
                attribution: BTreeMap::new(),
                health: ResourceHealth {
                    native: ResourceSourceHealth {
                        status: ResourceSourceStatus::Starting,
                        ..Default::default()
                    },
                    desktop: ResourceSourceHealth {
                        status: ResourceSourceStatus::Unavailable,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                lifecycle: ResourceLifecycleCounters::default(),
            }),
        }
    }

    fn snapshot(&self, power: HostPowerSnapshot) -> ResourceTelemetrySnapshot {
        let sampled_at = now();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let sampling_started = Instant::now();
        expire_desktop_health(&mut state.health, &sampled_at);
        let host = sample_host_resources(&mut state, &sampled_at);
        let process_probe = sample_processes(&mut state, &sampled_at);
        let mut processes = process_probe.processes;
        if state
            .desktop_processes_sampled_at
            .as_ref()
            .is_some_and(|sampled| {
                sampled_at.millis().saturating_sub(sampled.millis())
                    <= DESKTOP_POWER_HEALTH_TIMEOUT_MS
            })
        {
            processes.extend(state.desktop_processes.iter().cloned());
        }
        bound_resource_processes(&mut processes, std::process::id());
        let previous = state.snapshots.back().map(|snapshot| {
            (
                snapshot.read_at.millis(),
                snapshot
                    .processes
                    .iter()
                    .map(|process| (process.identity.key(), process.clone()))
                    .collect::<BTreeMap<_, _>>(),
            )
        });
        for process in &mut processes {
            let Some((previous_at, previous_processes)) = previous.as_ref() else {
                continue;
            };
            let Some(previous_process) = previous_processes.get(&process.identity.key()) else {
                continue;
            };
            let elapsed_ms = sampled_at.millis().saturating_sub(*previous_at);
            if (1..=30_000).contains(&elapsed_ms) {
                let cpu_delta = process
                    .cpu_time_ms
                    .saturating_sub(previous_process.cpu_time_ms);
                process.cpu_percent = (cpu_delta as f64 * 100.0 / elapsed_ms as f64).max(0.0);
                let read_delta = process
                    .io_read_bytes
                    .saturating_sub(previous_process.io_read_bytes);
                let write_delta = process
                    .io_write_bytes
                    .saturating_sub(previous_process.io_write_bytes);
                process.io_read_bytes_per_second = read_delta as f64 * 1_000.0 / elapsed_ms as f64;
                process.io_write_bytes_per_second =
                    write_delta as f64 * 1_000.0 / elapsed_ms as f64;
            }
            process.first_seen_at = previous_process.first_seen_at.clone();
            process.peak_resident_bytes = process
                .peak_resident_bytes
                .max(previous_process.peak_resident_bytes);
        }
        let empty_previous = BTreeMap::new();
        update_lifecycle_counters(
            &mut state.lifecycle,
            &processes,
            previous
                .as_ref()
                .map_or(&empty_previous, |(_, processes)| processes),
        );
        state.health.native.status = match process_probe.status {
            ResourceSourceStatus::Healthy if host.usable_for_load_balancing() => {
                ResourceSourceStatus::Healthy
            }
            ResourceSourceStatus::Healthy => ResourceSourceStatus::Degraded,
            status => status,
        };
        state.health.native.last_sample_at = Some(sampled_at.clone());
        state.health.native.last_error = process_probe.error.or_else(|| {
            (!host.usable_for_load_balancing())
                .then(|| "Native host resource capacity is unavailable.".into())
        });
        state.health.retained_process_count = processes.len() as u64;
        state.health.scanned_process_count = process_probe.scanned_process_count;
        state.health.inaccessible_process_count = process_probe.inaccessible_process_count;
        state.health.collection_duration_micros = sampling_started.elapsed().as_micros() as u64;
        record_attribution(
            &mut state.attribution,
            "host-daemon",
            "resource.sample",
            0,
            0,
            1,
            sampling_started.elapsed().as_millis() as u64,
        );
        let attribution = attribution_snapshot(&state, sampled_at.clone());
        let groups = resource_groups(&processes, &state.lifecycle);
        let speed_limit_percent = power.speed_limit_percent.map(f64::from);
        let snapshot = ResourceTelemetrySnapshot {
            read_at: sampled_at.clone(),
            sample_interval_ms: 5_000,
            processes,
            groups,
            power,
            speed_limit_percent,
            attribution,
            health: state.health.clone(),
        };
        state.snapshots.push_back(snapshot.clone());
        while state.snapshots.len() > RESOURCE_HISTORY_LIMIT
            || resource_history_entries(&state.snapshots) > RESOURCE_HISTORY_MAX_ENTRIES
            || state.snapshots.front().is_some_and(|oldest| {
                sampled_at.millis().saturating_sub(oldest.read_at.millis())
                    > RESOURCE_HISTORY_MAX_AGE_MS
            })
            || resource_history_bytes(&state.snapshots) > RESOURCE_HISTORY_MAX_BYTES
        {
            state.snapshots.pop_front();
        }
        snapshot
    }

    fn latest_history(
        &self,
        power: HostPowerSnapshot,
        window_ms: u64,
        bucket_ms: u64,
    ) -> ResourceTelemetryHistory {
        let (window_ms, bucket_ms) = normalize_resource_history_window(window_ms, bucket_ms);
        let snapshot = self.snapshot(power);
        let read_at_ms = snapshot.read_at.millis();
        let cutoff = read_at_ms.saturating_sub(window_ms as i64);
        let retained: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshots
            .iter()
            .filter(|sample| sample.read_at.millis() <= read_at_ms)
            .cloned()
            .collect();
        let in_window: Vec<_> = retained
            .iter()
            .filter(|sample| sample.read_at.millis() >= cutoff)
            .cloned()
            .collect();
        let preceding = retained
            .iter()
            .rfind(|sample| sample.read_at.millis() < cutoff);
        let mut previous = preceding;
        let mut aggregate_samples = Vec::new();
        let mut process_samples = Vec::new();
        for sample in &in_window {
            let delta_fraction = previous
                .filter(|previous| previous.read_at.millis() < cutoff)
                .and_then(|previous| {
                    let elapsed = sample
                        .read_at
                        .millis()
                        .saturating_sub(previous.read_at.millis());
                    (elapsed > 0).then(|| {
                        (sample.read_at.millis().saturating_sub(cutoff) as f64 / elapsed as f64)
                            .clamp(0.0, 1.0)
                    })
                })
                .unwrap_or(1.0);
            let (aggregate, processes) = history_sample(previous, sample, delta_fraction);
            aggregate_samples.push(aggregate);
            process_samples.extend(processes);
            previous = Some(sample);
        }
        let buckets = history_buckets(&aggregate_samples, read_at_ms, window_ms, bucket_ms);
        let top_processes = process_history_summaries(&process_samples);
        ResourceTelemetryHistory {
            read_at: snapshot.read_at,
            window_ms,
            bucket_ms,
            sample_interval_ms: snapshot.sample_interval_ms,
            retained_sample_count: (aggregate_samples.len() + process_samples.len()) as u64,
            buckets,
            top_processes,
            health: snapshot.health,
        }
    }

    fn host_resources(&self) -> agent_domain::HostResourcesSnapshot {
        let sampled_at = now();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let (Some(snapshot), Some(refreshed_at)) = (
            state.host_resources.clone(),
            state.host_resources_refreshed_at,
        ) && refreshed_at.elapsed() < Duration::from_secs(5)
        {
            return snapshot;
        }
        let sampled = sample_host_resources(&mut state, &sampled_at);
        record_attribution(
            &mut state.attribution,
            "host-daemon",
            "host-resources.read",
            0,
            0,
            1,
            0,
        );
        sampled
    }

    fn record_attribution(
        &self,
        component: &str,
        operation: &str,
        logical_read_bytes: u64,
        logical_write_bytes: u64,
        count: u64,
        duration_ms: u64,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        record_attribution(
            &mut state.attribution,
            component,
            operation,
            logical_read_bytes,
            logical_write_bytes,
            count,
            duration_ms,
        );
    }

    fn record_desktop_power(&self, snapshot: &HostPowerSnapshot) {
        if snapshot.source != HostPowerSource::DesktopMain {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.desktop_processes = snapshot.desktop_processes.clone();
        state.desktop_processes_sampled_at =
            (!state.desktop_processes.is_empty()).then(|| snapshot.updated_at.clone());
        let unavailable = snapshot.stale || !has_power_observation(snapshot);
        state.health.desktop.status = if unavailable {
            ResourceSourceStatus::Unavailable
        } else {
            ResourceSourceStatus::Healthy
        };
        state.health.desktop.last_sample_at = (!unavailable).then(|| snapshot.updated_at.clone());
        state.health.desktop.last_error = unavailable
            .then(|| "The desktop publisher did not provide a fresh native observation.".into());
    }
}

/// Samples the GPUI desktop process tree on its own blocking owner. The Host
/// cannot discover this tree through descendant scanning because the desktop
/// and Host are sibling processes.
pub struct DesktopProcessMonitor {
    owner: ResourceOwner,
}

impl DesktopProcessMonitor {
    pub fn new() -> Self {
        Self {
            owner: ResourceOwner::new(),
        }
    }

    pub fn sample(&self) -> Vec<ResourceProcess> {
        let mut processes = self.owner.snapshot(unknown_power(now())).processes;
        for process in &mut processes {
            // GPUI is a native desktop process, so keep it as the canonical
            // unknown application category instead of claiming Electron
            // ownership from a command-line guess.
            process.category = ResourceProcessCategory::Unknown;
        }
        processes
    }
}

impl Default for DesktopProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

fn has_power_observation(snapshot: &HostPowerSnapshot) -> bool {
    snapshot.idle != BackgroundBooleanState::Unknown
        || snapshot.locked != BackgroundBooleanState::Unknown
        || snapshot.on_battery != BackgroundBooleanState::Unknown
        || snapshot.low_power_mode != BackgroundBooleanState::Unknown
        || snapshot.thermal_state != agent_domain::HostPowerThermalState::Unknown
        || snapshot.speed_limit_percent.is_some()
}

fn desktop_power_sample_is_fresh(snapshot: &HostPowerSnapshot, at: &Timestamp) -> bool {
    snapshot.source == HostPowerSource::DesktopMain
        && !snapshot.stale
        && has_power_observation(snapshot)
        && snapshot.updated_at.millis() <= at.millis()
        && at.millis().saturating_sub(snapshot.updated_at.millis())
            <= DESKTOP_POWER_HEALTH_TIMEOUT_MS
}

fn sanitize_desktop_processes(
    processes: &[ResourceProcess],
    sampled_at: &Timestamp,
) -> Vec<ResourceProcess> {
    let mut processes = processes.to_vec();
    for process in &mut processes {
        process.name = bounded_text(&process.name);
        process.command = bounded_text(&process.command);
        process.status = bounded_text(&process.status);
        process.child_pids.truncate(RESOURCE_CHILD_PID_LIMIT);
        if process.first_seen_at.millis() > sampled_at.millis() {
            process.first_seen_at = sampled_at.clone();
        }
        if process.last_seen_at.millis() > sampled_at.millis() {
            process.last_seen_at = sampled_at.clone();
        }
    }
    bound_resource_processes(&mut processes, std::process::id());
    processes
}

fn expire_desktop_health(health: &mut ResourceHealth, at: &Timestamp) {
    if health
        .desktop
        .last_sample_at
        .as_ref()
        .is_some_and(|sampled| {
            at.millis().saturating_sub(sampled.millis()) > DESKTOP_POWER_HEALTH_TIMEOUT_MS
        })
    {
        health.desktop.status = ResourceSourceStatus::Unavailable;
        health.desktop.last_error =
            Some("The desktop power publisher has not reported a fresh observation.".into());
    }
}

fn sample_host_resources(
    state: &mut ResourceState,
    sampled_at: &Timestamp,
) -> agent_domain::HostResourcesSnapshot {
    state.system.refresh_memory();
    let total_memory_bytes = state.system.total_memory();
    #[cfg(target_os = "linux")]
    let available_memory_bytes = {
        let mut available = state.system.available_memory();
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            if let Some(value) = parse_meminfo_bytes(&meminfo, "MemAvailable:") {
                available = value;
            }
        }
        available
    };
    #[cfg(not(target_os = "linux"))]
    let available_memory_bytes = state.system.available_memory();
    let previous_cpu_at = state.host_cpu_refreshed_at.replace(Instant::now());
    state.system.refresh_cpu_usage();
    let cpu_utilization = previous_cpu_at
        .filter(|at| at.elapsed() >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL)
        .and_then(|_| {
            let value = state.system.global_cpu_usage() as f64 / 100.0;
            value.is_finite().then(|| value.clamp(0.0, 1.0))
        });
    let snapshot = agent_domain::HostResourcesSnapshot {
        sampled_at: sampled_at.millis().max(0) as u64,
        cpu_utilization,
        cpu_count: state.system.cpus().len() as u64,
        available_memory_bytes: available_memory_bytes.min(total_memory_bytes),
        total_memory_bytes,
    };
    state.host_resources = Some(snapshot.clone());
    state.host_resources_refreshed_at = Some(Instant::now());
    snapshot
}

#[cfg(target_os = "linux")]
fn parse_meminfo_bytes(contents: &str, name: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next()? == name)
            .then(|| parts.next()?.parse::<u64>().ok()?.saturating_mul(1024))
            .flatten()
    })
}

struct ProcessProbe {
    processes: Vec<ResourceProcess>,
    scanned_process_count: u64,
    inaccessible_process_count: u64,
    status: ResourceSourceStatus,
    error: Option<String>,
}

fn sample_processes(state: &mut ResourceState, at: &Timestamp) -> ProcessProbe {
    let refresh_kind = ProcessRefreshKind::new()
        .with_memory()
        .with_cpu()
        .with_disk_usage()
        .with_cmd(UpdateKind::Always);
    state
        .system
        .refresh_processes_specifics(ProcessesToUpdate::All, refresh_kind);
    let scanned_process_count = state.system.processes().len() as u64;
    let root_pid = std::process::id();
    let rows = state
        .system
        .processes()
        .iter()
        .map(|(pid, process)| {
            (
                pid.as_u32(),
                process.parent().map(|pid| pid.as_u32()).unwrap_or(0),
                process.start_time().saturating_mul(1_000),
            )
        })
        .collect::<Vec<_>>();
    let selected = select_tracked_pids(&rows, root_pid);
    if !rows.iter().any(|(pid, _, _)| *pid == root_pid) {
        return ProcessProbe {
            processes: vec![],
            scanned_process_count,
            inaccessible_process_count: selected.len() as u64,
            status: ResourceSourceStatus::Unavailable,
            error: Some("The Host process was not present in the native process table.".into()),
        };
    }

    let previous_at = state
        .snapshots
        .back()
        .map(|snapshot| snapshot.read_at.millis());
    let mut processes = Vec::with_capacity(selected.len());
    for pid in selected {
        let Some(process) = state.system.process(sysinfo::Pid::from_u32(pid)) else {
            continue;
        };
        let start_time_ms = process.start_time().saturating_mul(1_000);
        let identity = ResourceProcessIdentity { pid, start_time_ms };
        let identity_key = identity.key();
        let cpu_percent = f64::from(process.cpu_usage());
        let cpu_percent = if cpu_percent.is_finite() {
            cpu_percent.max(0.0)
        } else {
            0.0
        };
        let cpu_time_ms = state
            .process_cpu_time_ms
            .get(&identity_key)
            .copied()
            .unwrap_or(0)
            .saturating_add(
                previous_at
                    .and_then(|previous| {
                        let elapsed = at.millis().saturating_sub(previous);
                        (elapsed > 0 && elapsed <= MAX_RESOURCE_DELTA_INTERVAL_MS)
                            .then(|| (cpu_percent * elapsed as f64 / 100.0).round().max(0.0) as u64)
                    })
                    .unwrap_or(0),
            );
        state.process_cpu_time_ms.insert(identity_key, cpu_time_ms);
        let raw_name = process.name().to_string_lossy();
        let name = if raw_name.is_empty() {
            "unknown".to_owned()
        } else {
            bounded_text(&raw_name)
        };
        let command = if process.cmd().is_empty() {
            name.clone()
        } else {
            bounded_text(
                &process
                    .cmd()
                    .iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        };
        let lower = command.to_ascii_lowercase();
        let category = process_category(pid, root_pid, &lower);
        let disk = process.disk_usage();
        processes.push(ResourceProcess {
            identity,
            ppid: process.parent().map(|pid| pid.as_u32()).unwrap_or(0),
            child_pids: vec![],
            depth: 0,
            name,
            command,
            status: bounded_text(&format!("{:?}", process.status())),
            category,
            cpu_percent,
            cpu_time_ms,
            resident_bytes: process.memory(),
            peak_resident_bytes: process.memory(),
            virtual_bytes: process.virtual_memory(),
            io_read_bytes: disk.total_read_bytes,
            io_write_bytes: disk.total_written_bytes,
            io_read_bytes_per_second: 0.0,
            io_write_bytes_per_second: 0.0,
            io_semantics: if cfg!(target_os = "windows") {
                ResourceTelemetryIoSemantics::AllIo
            } else {
                ResourceTelemetryIoSemantics::Storage
            },
            run_time_ms: process.run_time().saturating_mul(1_000),
            first_seen_at: at.clone(),
            last_seen_at: at.clone(),
        });
    }
    let parent_map = processes
        .iter()
        .map(|process| (process.identity.pid, process.ppid))
        .collect::<BTreeMap<_, _>>();
    let child_pids = processes
        .iter()
        .map(|process| {
            (
                process.identity.pid,
                processes
                    .iter()
                    .filter(|candidate| candidate.ppid == process.identity.pid)
                    .map(|candidate| candidate.identity.pid)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for process in &mut processes {
        process.depth = process_depth(process.identity.pid, root_pid, &parent_map);
        process.child_pids = child_pids
            .get(&process.identity.pid)
            .cloned()
            .unwrap_or_default();
    }
    bound_resource_processes(&mut processes, root_pid);
    state.process_cpu_time_ms.retain(|key, _| {
        processes
            .iter()
            .any(|process| process.identity.key() == *key)
    });
    let inaccessible_process_count =
        selected_process_count(&rows, root_pid).saturating_sub(processes.len()) as u64;
    let (status, error) = if inaccessible_process_count > 0 {
        (
            ResourceSourceStatus::Degraded,
            Some(format!(
                "{} tracked descendant process(es) were unavailable or outside the bounded resource view.",
                inaccessible_process_count
            )),
        )
    } else {
        (ResourceSourceStatus::Healthy, None)
    };
    ProcessProbe {
        processes,
        scanned_process_count,
        inaccessible_process_count,
        status,
        error,
    }
}

fn bound_resource_processes(processes: &mut Vec<ResourceProcess>, root_pid: u32) {
    processes.sort_by(|left, right| {
        (right.identity.pid == root_pid || right.depth == 0)
            .cmp(&(left.identity.pid == root_pid || left.depth == 0))
            .then_with(|| right.resident_bytes.cmp(&left.resident_bytes))
            .then_with(|| left.identity.cmp(&right.identity))
    });
    processes.truncate(RESOURCE_PROCESS_LIMIT);
    let retained = processes
        .iter()
        .map(|process| process.identity.pid)
        .collect::<BTreeSet<_>>();
    for process in processes {
        process.child_pids.retain(|pid| retained.contains(pid));
        process.child_pids.truncate(RESOURCE_CHILD_PID_LIMIT);
    }
}

fn record_attribution(
    entries: &mut BTreeMap<(String, String), agent_domain::ResourceAttributionEntry>,
    component: &str,
    operation: &str,
    logical_read_bytes: u64,
    logical_write_bytes: u64,
    count: u64,
    duration_ms: u64,
) {
    let component = bounded_text(component);
    let operation = bounded_text(operation);
    let key = (component.clone(), operation.clone());
    let entry = entries
        .entry(key)
        .or_insert_with(|| agent_domain::ResourceAttributionEntry {
            component,
            operation,
            logical_read_bytes: 0,
            logical_write_bytes: 0,
            count: 0,
            duration_ms: 0,
        });
    entry.logical_read_bytes = entry.logical_read_bytes.saturating_add(logical_read_bytes);
    entry.logical_write_bytes = entry
        .logical_write_bytes
        .saturating_add(logical_write_bytes);
    entry.count = entry.count.saturating_add(count);
    entry.duration_ms = entry.duration_ms.saturating_add(duration_ms);
    while entries.len() > RESOURCE_ATTRIBUTION_LIMIT {
        let Some(key) = entries
            .iter()
            .min_by_key(|(_, entry)| {
                entry
                    .logical_read_bytes
                    .saturating_add(entry.logical_write_bytes)
            })
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        entries.remove(&key);
    }
}

fn attribution_snapshot(state: &ResourceState, read_at: Timestamp) -> ResourceAttributionSnapshot {
    let mut entries = state.attribution.values().cloned().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .logical_write_bytes
            .cmp(&left.logical_write_bytes)
            .then_with(|| right.logical_read_bytes.cmp(&left.logical_read_bytes))
            .then_with(|| left.component.cmp(&right.component))
            .then_with(|| left.operation.cmp(&right.operation))
    });
    entries.truncate(RESOURCE_ATTRIBUTION_LIMIT);
    ResourceAttributionSnapshot { read_at, entries }
}

fn bounded_text(value: &str) -> String {
    if value.chars().count() <= RESOURCE_TEXT_LIMIT {
        return value.to_owned();
    }
    value.chars().take(RESOURCE_TEXT_LIMIT).collect()
}

fn resource_snapshot_bytes(snapshot: &ResourceTelemetrySnapshot) -> usize {
    let process_bytes = snapshot.processes.iter().fold(0usize, |total, process| {
        total
            .saturating_add(process.name.len())
            .saturating_add(process.command.len())
            .saturating_add(process.status.len())
            .saturating_add(process.child_pids.len() * std::mem::size_of::<u32>())
            .saturating_add(256)
    });
    let attribution_bytes = snapshot
        .attribution
        .entries
        .iter()
        .fold(0usize, |total, entry| {
            total
                .saturating_add(entry.component.len())
                .saturating_add(entry.operation.len())
                .saturating_add(64)
        });
    process_bytes
        .saturating_add(attribution_bytes)
        .saturating_add(512)
}

fn resource_history_bytes(snapshots: &VecDeque<ResourceTelemetrySnapshot>) -> usize {
    snapshots.iter().map(resource_snapshot_bytes).sum()
}

fn resource_history_entries(snapshots: &VecDeque<ResourceTelemetrySnapshot>) -> usize {
    snapshots
        .iter()
        .map(|snapshot| snapshot.processes.len())
        .sum()
}

fn process_category(pid: u32, root_pid: u32, command: &str) -> ResourceProcessCategory {
    if pid == root_pid {
        ResourceProcessCategory::Server
    } else if command.contains("provider") {
        ResourceProcessCategory::ProviderRoot
    } else if command.contains("terminal") || command.contains("pty") {
        ResourceProcessCategory::TerminalRoot
    } else if command.contains("electron") && command.contains("renderer") {
        ResourceProcessCategory::ElectronRenderer
    } else if command.contains("electron") && command.contains("gpu") {
        ResourceProcessCategory::ElectronGpu
    } else if command.contains("electron") && command.contains("utility") {
        ResourceProcessCategory::ElectronUtility
    } else if command.contains("electron") {
        ResourceProcessCategory::ElectronMain
    } else if command.contains("resource-monitor") || command.contains("resource_monitor") {
        ResourceProcessCategory::ResourceMonitor
    } else {
        ResourceProcessCategory::ServerChild
    }
}

fn select_tracked_pids(rows: &[(u32, u32, u64)], root_pid: u32) -> BTreeSet<u32> {
    let mut children = BTreeMap::<u32, Vec<(u32, u64)>>::new();
    let mut starts = BTreeMap::<u32, u64>::new();
    for (pid, ppid, start_time_ms) in rows {
        children
            .entry(*ppid)
            .or_default()
            .push((*pid, *start_time_ms));
        starts.insert(*pid, *start_time_ms);
    }
    let Some(root_start) = starts.get(&root_pid).copied() else {
        return BTreeSet::new();
    };
    let mut tracked = BTreeSet::new();
    let mut queue = VecDeque::from([(root_pid, root_start)]);
    while let Some((pid, start_time_ms)) = queue.pop_front() {
        if !tracked.insert(pid) {
            continue;
        }
        queue.extend(
            children
                .get(&pid)
                .into_iter()
                .flatten()
                .copied()
                .filter(|(_, child_start)| *child_start >= start_time_ms),
        );
    }
    tracked
}

fn selected_process_count(rows: &[(u32, u32, u64)], root_pid: u32) -> usize {
    select_tracked_pids(rows, root_pid).len()
}

fn process_depth(pid: u32, root_pid: u32, parents: &BTreeMap<u32, u32>) -> u32 {
    if pid == root_pid {
        return 0;
    }
    let mut depth = 1;
    let mut parent = parents.get(&pid).copied().unwrap_or(0);
    while parent != 0 && parent != root_pid && depth < 64 {
        depth += 1;
        parent = parents.get(&parent).copied().unwrap_or(0);
    }
    depth
}

const MAX_RESOURCE_DELTA_INTERVAL_MS: i64 = 30_000;

struct HistoryAggregateSample {
    sampled_at_ms: i64,
    cpu_percent: f64,
    rss_bytes: u64,
    process_count: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
}

struct HistoryProcessSample {
    process: ResourceProcess,
    cpu_time_ms: u64,
    io_read_bytes: u64,
    io_write_bytes: u64,
}

fn is_backend_category(category: ResourceProcessCategory) -> bool {
    matches!(
        category,
        ResourceProcessCategory::Server
            | ResourceProcessCategory::ServerChild
            | ResourceProcessCategory::ProviderRoot
            | ResourceProcessCategory::TerminalRoot
    )
}

fn history_delta(current: u64, previous: Option<u64>, elapsed_ms: i64) -> u64 {
    if !(1..=MAX_RESOURCE_DELTA_INTERVAL_MS).contains(&elapsed_ms) {
        return 0;
    }
    previous
        .map(|previous| current.saturating_sub(previous))
        .unwrap_or(0)
}

fn scale_history_delta(delta: u64, fraction: f64) -> u64 {
    if fraction >= 1.0 {
        return delta;
    }
    (delta as f64 * fraction)
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

fn history_sample(
    previous: Option<&ResourceTelemetrySnapshot>,
    current: &ResourceTelemetrySnapshot,
    delta_fraction: f64,
) -> (HistoryAggregateSample, Vec<HistoryProcessSample>) {
    let elapsed_ms = previous.map_or(0, |sample| {
        current
            .read_at
            .millis()
            .saturating_sub(sample.read_at.millis())
    });
    let previous_processes = previous.map(|sample| {
        sample
            .processes
            .iter()
            .map(|process| (process.identity.key(), process))
            .collect::<BTreeMap<_, _>>()
    });
    let mut io_read_bytes: u64 = 0;
    let mut io_write_bytes: u64 = 0;
    let processes = current
        .processes
        .iter()
        .map(|process| {
            let previous = previous_processes
                .as_ref()
                .and_then(|processes| processes.get(&process.identity.key()).copied());
            let cpu_time_ms = scale_history_delta(
                history_delta(
                    process.cpu_time_ms,
                    previous.map(|process| process.cpu_time_ms),
                    elapsed_ms,
                ),
                delta_fraction,
            );
            let io_read = scale_history_delta(
                history_delta(
                    process.io_read_bytes,
                    previous.map(|process| process.io_read_bytes),
                    elapsed_ms,
                ),
                delta_fraction,
            );
            let io_write = scale_history_delta(
                history_delta(
                    process.io_write_bytes,
                    previous.map(|process| process.io_write_bytes),
                    elapsed_ms,
                ),
                delta_fraction,
            );
            if is_backend_category(process.category) {
                io_read_bytes = io_read_bytes.saturating_add(io_read);
                io_write_bytes = io_write_bytes.saturating_add(io_write);
            }
            HistoryProcessSample {
                process: process.clone(),
                cpu_time_ms,
                io_read_bytes: io_read,
                io_write_bytes: io_write,
            }
        })
        .collect();
    (
        HistoryAggregateSample {
            sampled_at_ms: current.read_at.millis(),
            cpu_percent: current.groups.backend.current_cpu_percent,
            rss_bytes: current.groups.backend.current_rss_bytes,
            process_count: current.groups.backend.process_count,
            io_read_bytes,
            io_write_bytes,
        },
        processes,
    )
}

fn history_buckets(
    samples: &[HistoryAggregateSample],
    read_at_ms: i64,
    window_ms: u64,
    bucket_ms: u64,
) -> Vec<ResourceHistoryBucket> {
    let window_start_ms = read_at_ms.saturating_sub(window_ms as i64);
    let mut buckets = Vec::new();
    let mut started_ms = window_start_ms;
    while started_ms < read_at_ms {
        let ended_ms = read_at_ms.min(started_ms.saturating_add(bucket_ms as i64));
        let bucket_samples: Vec<_> = samples
            .iter()
            .filter(|sample| {
                let at = sample.sampled_at_ms;
                at >= started_ms && (ended_ms == read_at_ms || at < ended_ms)
            })
            .collect();
        let count = bucket_samples.len() as f64;
        buckets.push(ResourceHistoryBucket {
            started_at: Timestamp::from_millis(started_ms).expect("history bucket start is valid"),
            ended_at: Timestamp::from_millis(ended_ms).expect("history bucket end is valid"),
            avg_cpu_percent: if count == 0.0 {
                0.0
            } else {
                bucket_samples
                    .iter()
                    .map(|sample| sample.cpu_percent)
                    .sum::<f64>()
                    / count
            },
            max_cpu_percent: bucket_samples
                .iter()
                .map(|sample| sample.cpu_percent)
                .fold(0.0, f64::max),
            max_rss_bytes: bucket_samples
                .iter()
                .map(|sample| sample.rss_bytes)
                .max()
                .unwrap_or(0),
            io_read_bytes: bucket_samples
                .iter()
                .map(|sample| sample.io_read_bytes)
                .sum(),
            io_write_bytes: bucket_samples
                .iter()
                .map(|sample| sample.io_write_bytes)
                .sum(),
            max_process_count: bucket_samples
                .iter()
                .map(|sample| sample.process_count)
                .max()
                .unwrap_or(0),
        });
        started_ms = ended_ms;
    }
    buckets
}

fn process_history_summaries(samples: &[HistoryProcessSample]) -> Vec<ResourceProcessSummary> {
    let mut summaries = BTreeMap::<String, ResourceProcessSummary>::new();
    for sample in samples {
        let process = &sample.process;
        let entry =
            summaries
                .entry(process.identity.key())
                .or_insert_with(|| ResourceProcessSummary {
                    identity: process.identity.clone(),
                    ppid: process.ppid,
                    depth: process.depth,
                    name: process.name.clone(),
                    command: process.command.clone(),
                    category: process.category,
                    first_seen_at: process.first_seen_at.clone(),
                    last_seen_at: process.last_seen_at.clone(),
                    current_cpu_percent: process.cpu_percent,
                    avg_cpu_percent: 0.0,
                    max_cpu_percent: process.cpu_percent,
                    cpu_time_ms: 0,
                    current_rss_bytes: process.resident_bytes,
                    peak_rss_bytes: process.resident_bytes,
                    io_read_bytes: 0,
                    io_write_bytes: 0,
                    io_semantics: process.io_semantics,
                    sample_count: 0,
                });
        entry.sample_count += 1;
        entry.avg_cpu_percent += process.cpu_percent;
        entry.current_cpu_percent = process.cpu_percent;
        entry.max_cpu_percent = entry.max_cpu_percent.max(process.cpu_percent);
        entry.cpu_time_ms = entry.cpu_time_ms.saturating_add(sample.cpu_time_ms);
        entry.current_rss_bytes = process.resident_bytes;
        entry.peak_rss_bytes = entry.peak_rss_bytes.max(process.resident_bytes);
        entry.io_read_bytes = entry.io_read_bytes.saturating_add(sample.io_read_bytes);
        entry.io_write_bytes = entry.io_write_bytes.saturating_add(sample.io_write_bytes);
        entry.last_seen_at = process.last_seen_at.clone();
    }
    let mut values: Vec<_> = summaries.into_values().collect();
    for entry in &mut values {
        entry.avg_cpu_percent /= entry.sample_count.max(1) as f64;
    }
    values.sort_by(|left, right| {
        right
            .cpu_time_ms
            .cmp(&left.cpu_time_ms)
            .then_with(|| right.peak_rss_bytes.cmp(&left.peak_rss_bytes))
            .then_with(|| left.identity.cmp(&right.identity))
    });
    values.truncate(agent_domain::RESOURCE_HISTORY_MAX_TOP_PROCESSES);
    values
}

fn aggregate_values<'a>(processes: impl Iterator<Item = &'a ResourceProcess>) -> ResourceAggregate {
    let mut aggregate = ResourceAggregate::default();
    for process in processes {
        aggregate.process_count += 1;
        aggregate.current_cpu_percent += process.cpu_percent;
        aggregate.cpu_time_ms = aggregate.cpu_time_ms.saturating_add(process.cpu_time_ms);
        aggregate.current_rss_bytes = aggregate
            .current_rss_bytes
            .saturating_add(process.resident_bytes);
        aggregate.peak_rss_bytes = aggregate
            .peak_rss_bytes
            .saturating_add(process.peak_resident_bytes);
        aggregate.io_read_bytes = aggregate
            .io_read_bytes
            .saturating_add(process.io_read_bytes);
        aggregate.io_write_bytes = aggregate
            .io_write_bytes
            .saturating_add(process.io_write_bytes);
        aggregate.io_read_bytes_per_second += process.io_read_bytes_per_second;
        aggregate.io_write_bytes_per_second += process.io_write_bytes_per_second;
    }
    aggregate
}

fn lifecycle_group_mut(
    counters: &mut ResourceLifecycleCounters,
    category: ResourceProcessCategory,
) -> &mut ResourceLifecycleGroup {
    if matches!(
        category,
        ResourceProcessCategory::ElectronMain
            | ResourceProcessCategory::ElectronRenderer
            | ResourceProcessCategory::ElectronGpu
            | ResourceProcessCategory::ElectronUtility
    ) {
        &mut counters.electron
    } else if category == ResourceProcessCategory::ResourceMonitor {
        &mut counters.monitor
    } else {
        &mut counters.backend
    }
}

fn update_lifecycle_counters(
    counters: &mut ResourceLifecycleCounters,
    processes: &[ResourceProcess],
    previous: &BTreeMap<String, ResourceProcess>,
) {
    let current = processes
        .iter()
        .map(|process| (process.identity.key(), process))
        .collect::<BTreeMap<_, _>>();
    for (key, process) in &current {
        let is_new = !previous.contains_key(key);
        counters.all_processes.process_starts += u64::from(is_new);
        if let Some(previous) = previous.get(key) {
            let cpu_time_ms = process.cpu_time_ms.saturating_sub(previous.cpu_time_ms);
            let io_read_bytes = process.io_read_bytes.saturating_sub(previous.io_read_bytes);
            let io_write_bytes = process
                .io_write_bytes
                .saturating_sub(previous.io_write_bytes);
            counters.all_processes.cpu_time_ms = counters
                .all_processes
                .cpu_time_ms
                .saturating_add(cpu_time_ms);
            counters.all_processes.io_read_bytes = counters
                .all_processes
                .io_read_bytes
                .saturating_add(io_read_bytes);
            counters.all_processes.io_write_bytes = counters
                .all_processes
                .io_write_bytes
                .saturating_add(io_write_bytes);
            let group = lifecycle_group_mut(counters, process.category);
            group.cpu_time_ms = group.cpu_time_ms.saturating_add(cpu_time_ms);
            group.io_read_bytes = group.io_read_bytes.saturating_add(io_read_bytes);
            group.io_write_bytes = group.io_write_bytes.saturating_add(io_write_bytes);
        }
        let group = lifecycle_group_mut(counters, process.category);
        group.process_starts += u64::from(is_new);
    }
    for (key, previous) in previous {
        if current.contains_key(key) {
            continue;
        }
        let group = lifecycle_group_mut(counters, previous.category);
        group.process_exits += 1;
        counters.all_processes.process_exits += 1;
    }
}

fn apply_lifecycle(aggregate: &mut ResourceAggregate, counters: ResourceLifecycleGroup) {
    aggregate.cpu_time_ms = counters.cpu_time_ms;
    aggregate.io_read_bytes = counters.io_read_bytes;
    aggregate.io_write_bytes = counters.io_write_bytes;
    aggregate.process_starts = counters.process_starts;
    aggregate.process_exits = counters.process_exits;
}

fn resource_groups(
    processes: &[ResourceProcess],
    counters: &ResourceLifecycleCounters,
) -> agent_domain::ResourceGroups {
    let backend = aggregate_values(processes.iter().filter(|process| {
        matches!(
            process.category,
            ResourceProcessCategory::Server
                | ResourceProcessCategory::ServerChild
                | ResourceProcessCategory::ProviderRoot
                | ResourceProcessCategory::TerminalRoot
        )
    }));
    let electron = aggregate_values(processes.iter().filter(|process| {
        matches!(
            process.category,
            ResourceProcessCategory::ElectronMain
                | ResourceProcessCategory::ElectronRenderer
                | ResourceProcessCategory::ElectronGpu
                | ResourceProcessCategory::ElectronUtility
        )
    }));
    let monitor = aggregate_values(
        processes
            .iter()
            .filter(|process| process.category == ResourceProcessCategory::ResourceMonitor),
    );
    let all_processes = aggregate_values(processes.iter());
    let mut backend = backend;
    let mut electron = electron;
    let mut monitor = monitor;
    let mut all_processes = all_processes;
    apply_lifecycle(&mut backend, counters.backend);
    apply_lifecycle(&mut electron, counters.electron);
    apply_lifecycle(&mut monitor, counters.monitor);
    apply_lifecycle(&mut all_processes, counters.all_processes);
    agent_domain::ResourceGroups {
        backend,
        electron,
        monitor,
        all_processes,
    }
}

/// One in-process owner for policy, power, resource, and trace state.
pub(crate) struct BackgroundOwner {
    mutation: TokioMutex<()>,
    power_probe: TokioMutex<()>,
    resource_probe: TokioMutex<()>,
    probe_stop: CancellationToken,
    policy: RwLock<BackgroundActivityPolicy>,
    leases: RwLock<BTreeMap<String, ClientActivityLease>>,
    power: Arc<HostPowerMonitor>,
    changes: broadcast::Sender<BackgroundPolicySnapshot>,
    resources: Arc<ResourceOwner>,
    state_directory: PathBuf,
    policy_path: PathBuf,
}
impl BackgroundOwner {
    pub(crate) fn new(state_directory: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(state_directory.join("logs"));
        let policy_path = state_directory.join("background-policy.json");
        let policy = std::fs::read_to_string(&policy_path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_else(|| {
                BackgroundActivityPolicy::preset(agent_domain::BackgroundActivityProfile::Balanced)
            });
        let (changes, _) = broadcast::channel(16);
        Arc::new(Self {
            mutation: TokioMutex::new(()),
            power_probe: TokioMutex::new(()),
            resource_probe: TokioMutex::new(()),
            probe_stop: CancellationToken::new(),
            policy: RwLock::new(policy.normalized()),
            leases: RwLock::new(BTreeMap::new()),
            power: Arc::new(HostPowerMonitor::new()),
            changes,
            resources: Arc::new(ResourceOwner::new()),
            state_directory,
            policy_path,
        })
    }

    pub(crate) async fn policy(&self) -> BackgroundActivityPolicy {
        self.policy.read().await.clone()
    }

    pub(crate) async fn set_policy(
        &self,
        policy: BackgroundActivityPolicy,
    ) -> BackgroundPolicySnapshot {
        let _mutation = self.mutation.lock().await;
        let policy = policy.normalized();
        *self.policy.write().await = policy.clone();
        if let Err(error) = crate::platform::save_private_json(&self.policy_path, &policy) {
            tracing::warn!(target: "bex", operation = "background.policy.persist", message = %error);
        }
        self.publish().await
    }

    pub(crate) async fn report_power(
        &self,
        mut snapshot: HostPowerSnapshot,
        desktop_publisher_allowed: bool,
    ) -> BackgroundPolicySnapshot {
        let _mutation = self.mutation.lock().await;
        // RPC clients cannot choose the Host's ordering clock.  A future
        // client timestamp must not pin the monitor ahead of the Host's own
        // native observation or make the publisher health look fresh forever.
        let previous = self.power.snapshot().await;
        let received_at = now();
        // Keep the Host clock monotonic across a wall-clock rollback.  The
        // sample is still received now; retaining the last timestamp only
        // preserves ordering until the local clock catches up.
        let observed_at = if received_at < previous.updated_at {
            previous.updated_at.clone()
        } else {
            received_at
        };
        snapshot.updated_at = observed_at.clone();
        // All client supplied power belongs to the supervised local desktop
        // session.  A remote client must not replace this Host's native
        // observation by relabeling its machine as NodeLinux or ElectronMain.
        if !desktop_publisher_allowed {
            return self.snapshot_at(&observed_at).await;
        }
        // The desktop process tree is meaningful only on the supervised
        // DesktopMain publisher.  A Node power report must not smuggle a
        // client-supplied tree into the Host's diagnostics snapshot. Bound
        // the accepted tree before retaining it in the power monitor too.
        if snapshot.source == HostPowerSource::DesktopMain {
            snapshot.desktop_processes =
                sanitize_desktop_processes(&snapshot.desktop_processes, &snapshot.updated_at);
        } else {
            snapshot.desktop_processes.clear();
        }
        if snapshot.source == HostPowerSource::DesktopMain {
            self.record_desktop_power(&snapshot).await;
        }
        // A desktop publisher can lose access to its native probe while the
        // supervised Host still has a valid local observation.  Keep that
        // observation authoritative rather than replacing it with a fresh
        // timestamp that only says "the desktop probe failed".
        if snapshot.source == HostPowerSource::DesktopMain
            && (snapshot.stale || !has_power_observation(&snapshot))
            && previous.source != HostPowerSource::DesktopMain
            && !previous.stale
        {
            return self.snapshot_at(&observed_at).await;
        }
        if self.power.report(snapshot).await {
            self.publish().await
        } else {
            self.snapshot_at(&observed_at).await
        }
    }

    /// Returns whether a scope currently has a lease that may perform work.
    /// The caller owns the actual work and supplies its own interval clock.
    pub(crate) async fn should_run_scope_work(&self, scope: &BackgroundScope) -> bool {
        let at = now();
        let policy = self.policy().await;
        let power = self.power.snapshot().await;
        if host_power_constrained(&power, &policy) {
            return false;
        }
        self.leases
            .read()
            .await
            .values()
            .any(|lease| lease_may_run_scoped_work(lease, scope, &at, &policy))
    }

    /// Active VCS leases are the Host's source of truth for which checkouts
    /// may receive an automatic remote refresh. The returned paths are
    /// deduplicated before the owner starts Git work so several clients
    /// watching one checkout do not create parallel fetches.
    pub(crate) async fn demanded_vcs_workspaces(&self) -> Vec<String> {
        let at = now();
        let policy = self.policy().await;
        let power = self.power.snapshot().await;
        if host_power_constrained(&power, &policy) {
            return vec![];
        }
        let leases = self.leases.read().await;
        let mut workspaces = std::collections::BTreeSet::new();
        for lease in leases.values() {
            for scope in &lease.scopes {
                if matches!(scope, BackgroundScope::VcsStatus { .. })
                    && lease_may_run_scoped_work(lease, scope, &at, &policy)
                    && let BackgroundScope::VcsStatus { cwd } = scope
                    && !cwd.trim().is_empty()
                {
                    workspaces.insert(cwd.clone());
                }
            }
        }
        workspaces.into_iter().collect()
    }

    /// Provider health is one catalog operation, so a generic provider
    /// lease or an instance-specific lease is enough to refresh the live
    /// catalog. The provider owner still decides which instances are
    /// configured; this method only supplies the policy gate.
    pub(crate) async fn has_provider_status_demand(&self) -> bool {
        let at = now();
        let policy = self.policy().await;
        let power = self.power.snapshot().await;
        if host_power_constrained(&power, &policy) {
            return false;
        }
        let leases = self.leases.read().await;
        leases.values().any(|lease| {
            lease.scopes.iter().any(|scope| {
                matches!(scope, BackgroundScope::ProviderStatus { .. })
                    && lease_may_run_scoped_work(lease, scope, &at, &policy)
            })
        })
    }

    /// Resource history is sampled only while a diagnostics client has an
    /// active lease. Explicit diagnostics reads still take an immediate
    /// sample through their normal RPC path.
    pub(crate) async fn sample_resources_if_demanded(&self) {
        let scope = BackgroundScope::Diagnostics;
        if self.should_run_scope_work(&scope).await {
            let power = self.power.snapshot().await;
            let _ = self.sample_resources(power).await;
        }
    }

    async fn sample_resources(&self, power: HostPowerSnapshot) -> ResourceTelemetrySnapshot {
        let _probe = self.resource_probe.lock().await;
        let resources = self.resources.clone();
        tokio::task::spawn_blocking(move || resources.snapshot(power))
            .await
            .expect("resource sampling worker terminated")
    }

    async fn sample_resource_history(
        &self,
        power: HostPowerSnapshot,
        window_ms: u64,
        bucket_ms: u64,
    ) -> ResourceTelemetryHistory {
        let _probe = self.resource_probe.lock().await;
        let resources = self.resources.clone();
        tokio::task::spawn_blocking(move || resources.latest_history(power, window_ms, bucket_ms))
            .await
            .expect("resource history worker terminated")
    }

    pub(crate) async fn snapshot(&self) -> BackgroundPolicySnapshot {
        if self.publish_power_sample().await {
            let _ = self.publish().await;
        }
        self.snapshot_at(&now()).await
    }

    async fn snapshot_at(&self, at: &Timestamp) -> BackgroundPolicySnapshot {
        let policy = self.policy().await;
        let power = self.power.snapshot().await;
        let leases = self.leases.read().await.clone();
        compute_background_snapshot(power, &leases, at, &policy)
    }

    async fn publish(&self) -> BackgroundPolicySnapshot {
        let snapshot = self.snapshot_at(&now()).await;
        let _ = self.changes.send(snapshot.clone());
        snapshot
    }

    async fn publish_power_sample(&self) -> bool {
        let Ok(_probe) = self.power_probe.try_lock() else {
            return false;
        };
        let current = self.power.snapshot().await;
        if desktop_power_sample_is_fresh(&current, &now()) {
            return false;
        }
        let sampled = sample_local_power(&self.probe_stop).await;
        self.power.report(sampled).await
    }

    pub(crate) async fn report_activity(
        &self,
        session: u64,
        input: agent_protocol::background::ReportClientActivity,
    ) -> Result<BackgroundPolicySnapshot, String> {
        let _mutation = self.mutation.lock().await;
        let policy = self.policy().await;
        let now = now();
        let report = input.report;
        let lease = report
            .lease_at(
                format!("session:{session}"),
                input.rpc_client_id,
                &policy,
                &now,
            )
            .map_err(str::to_owned)?;
        let mut leases = self.leases.write().await;
        let next = upsert_client_activity_lease(&leases, lease, &now);
        *leases = next;
        drop(leases);
        Ok(self.publish().await)
    }

    pub(crate) async fn remove_activity(
        &self,
        session: u64,
        rpc_client_id: u64,
    ) -> BackgroundPolicySnapshot {
        let _mutation = self.mutation.lock().await;
        let mut leases = self.leases.write().await;
        let next = remove_rpc_client(&leases, &format!("session:{session}"), rpc_client_id);
        *leases = next;
        drop(leases);
        self.publish().await
    }

    pub(crate) async fn close_session(&self, session: u64) {
        let _mutation = self.mutation.lock().await;
        let mut leases = self.leases.write().await;
        let session_id = format!("session:{session}");
        leases.retain(|_, lease| lease.session_id != session_id);
        drop(leases);
        let _ = self.publish().await;
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<BackgroundPolicySnapshot> {
        self.changes.subscribe()
    }

    /// Attach a subscriber while the mutation owner is held so the initial
    /// snapshot and later semantic changes form one ordered stream.
    pub(crate) async fn subscribe_with_snapshot(
        &self,
    ) -> (
        broadcast::Receiver<BackgroundPolicySnapshot>,
        BackgroundPolicySnapshot,
    ) {
        let _mutation = self.mutation.lock().await;
        let snapshot = self.snapshot().await;
        (self.changes.subscribe(), snapshot)
    }

    pub(crate) async fn host_resources(&self) -> agent_domain::HostResourcesSnapshot {
        let _probe = self.resource_probe.lock().await;
        let resources = self.resources.clone();
        tokio::task::spawn_blocking(move || resources.host_resources())
            .await
            .expect("host resource worker terminated")
    }

    async fn record_desktop_power(&self, snapshot: &HostPowerSnapshot) {
        let _probe = self.resource_probe.lock().await;
        let resources = self.resources.clone();
        let snapshot = snapshot.clone();
        tokio::task::spawn_blocking(move || resources.record_desktop_power(&snapshot))
            .await
            .expect("desktop resource worker terminated");
    }

    pub(crate) async fn record_attribution(
        &self,
        component: &str,
        operation: &str,
        logical_read_bytes: u64,
        logical_write_bytes: u64,
        count: u64,
        duration_ms: u64,
    ) {
        let _probe = self.resource_probe.lock().await;
        let resources = self.resources.clone();
        let component = component.to_owned();
        let operation = operation.to_owned();
        tokio::task::spawn_blocking(move || {
            resources.record_attribution(
                &component,
                &operation,
                logical_read_bytes,
                logical_write_bytes,
                count,
                duration_ms,
            )
        })
        .await
        .expect("resource attribution worker terminated");
    }

    pub(crate) async fn process_diagnostics(&self) -> agent_domain::ProcessDiagnosticsResult {
        let snapshot = self.sample_resources(self.power.snapshot().await).await;
        process_diagnostics(
            std::process::id(),
            snapshot.read_at.clone(),
            &snapshot.processes,
            snapshot
                .health
                .native
                .last_error
                .map(|message| agent_domain::ProcessDiagnosticsError { message }),
        )
    }

    pub(crate) async fn process_history(
        &self,
        window_ms: u64,
        bucket_ms: u64,
    ) -> agent_domain::ProcessResourceHistoryResult {
        let history = self
            .sample_resource_history(self.power.snapshot().await, window_ms, bucket_ms)
            .await;
        project_process_resource_history(&history)
    }

    pub(crate) async fn trace_diagnostics(
        &self,
        input: &agent_protocol::background::ReadTraceDiagnostics,
    ) -> Result<TraceDiagnosticsResult, String> {
        let path = self.allowed_trace_path(&input.trace_file_path)?;
        let max_files = input.max_files.min(TRACE_MAX_FILES);
        let slow_span_threshold_ms = input.slow_span_threshold_ms.unwrap_or(1_000.0);
        if !slow_span_threshold_ms.is_finite() || slow_span_threshold_ms < 0.0 {
            return Err("slow span threshold must be finite and non-negative".into());
        }
        let mut paths = Vec::with_capacity(max_files as usize + 1);
        for index in (1..=max_files).rev() {
            paths.push(format!("{}.{}", path.display(), index));
        }
        paths.push(path.to_string_lossy().into_owned());
        let read_at = now();
        let trace_started = Instant::now();
        let mut logical_read_bytes = 0u64;
        let mut aggregator = TraceDiagnosticsAggregator::new(slow_span_threshold_ms);
        let mut found = false;
        let mut failure = None;
        for scanned in &paths {
            match tokio::fs::File::open(scanned).await {
                Ok(file) => {
                    found = true;
                    let mut lines = tokio::io::BufReader::new(file).lines();
                    loop {
                        match lines.next_line().await {
                            Ok(Some(line)) => {
                                logical_read_bytes =
                                    logical_read_bytes.saturating_add(line.len() as u64);
                                aggregator.add_line(&line);
                            }
                            Ok(None) => break,
                            Err(_error) => {
                                if failure.is_none() {
                                    failure = Some(TraceDiagnosticsError {
                                        kind: TraceDiagnosticsErrorKind::TraceFileReadFailed,
                                        message: format!(
                                            "Failed to read local trace file '{scanned}'."
                                        ),
                                    });
                                }
                                break;
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_error) => {
                    if failure.is_none() {
                        failure = Some(TraceDiagnosticsError {
                            kind: TraceDiagnosticsErrorKind::TraceFileReadFailed,
                            message: format!("Failed to read local trace file '{scanned}'."),
                        });
                    }
                }
            }
        }
        if !found {
            self.record_attribution(
                "diagnostics",
                "trace.read",
                logical_read_bytes,
                0,
                1,
                trace_started.elapsed().as_millis() as u64,
            )
            .await;
            return Ok(agent_domain::empty_trace_diagnostics(
                path.to_string_lossy(),
                paths,
                read_at,
                slow_span_threshold_ms,
                failure.unwrap_or(TraceDiagnosticsError {
                    kind: TraceDiagnosticsErrorKind::TraceFileNotFound,
                    message: "No local trace files were found.".into(),
                }),
            ));
        }
        let partial_failure = failure.as_ref().map(|_| true);
        self.record_attribution(
            "diagnostics",
            "trace.read",
            logical_read_bytes,
            0,
            1,
            trace_started.elapsed().as_millis() as u64,
        )
        .await;
        Ok(aggregator.finish(
            path.to_string_lossy(),
            paths,
            read_at,
            failure,
            partial_failure,
        ))
    }

    fn allowed_trace_path(&self, requested: &str) -> Result<PathBuf, String> {
        let default = self.state_directory.join("logs/host.jsonl");
        let path = if requested.trim().is_empty() {
            default
        } else {
            PathBuf::from(requested)
        };
        let parent = path
            .parent()
            .ok_or_else(|| "trace path has no parent".to_owned())?;
        let logs = self.state_directory.join("logs");
        let parent =
            dunce::canonicalize(parent).map_err(|_| "trace path is unavailable".to_owned())?;
        let logs =
            dunce::canonicalize(logs).map_err(|_| "trace directory is unavailable".to_owned())?;
        if !parent.starts_with(&logs) {
            return Err("trace path must be inside the Host diagnostics directory".into());
        }
        if let Ok(canonical) = dunce::canonicalize(&path)
            && !canonical.starts_with(&logs)
        {
            return Err("trace path must be inside the Host diagnostics directory".into());
        }
        Ok(path)
    }

    pub(crate) fn spawn(
        self: &Arc<Self>,
        stop: CancellationToken,
    ) -> tokio_util::task::AbortOnDropHandle<()> {
        let owner = Arc::downgrade(self);
        let probe_stop = self.probe_stop.clone();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut next_power_sample_ms = now().millis();
            let mut suspend_events = SuspendLifecycleSource::start(&stop);
            loop {
                tokio::select! {
                    _ = stop.cancelled() => {
                        probe_stop.cancel();
                        suspend_events.shutdown().await;
                        return;
                    },
                    event = suspend_events.recv() => {
                        let Some(owner) = owner.upgrade() else {
                            probe_stop.cancel();
                            suspend_events.shutdown().await;
                            return;
                        };
                        let Some(event) = event else {
                            probe_stop.cancel();
                            suspend_events.shutdown().await;
                            return;
                        };
                        let _mutation = owner.mutation.lock().await;
                        if owner.power.report_lifecycle(event.suspended()).await {
                            let _ = owner.publish().await;
                        }
                    },
                    _ = interval.tick() => {
                        let Some(owner) = owner.upgrade() else {
                            probe_stop.cancel();
                            suspend_events.shutdown().await;
                            return;
                        };
                        let (leases_changed, interval_ms, current_ms) = {
                            let _mutation = owner.mutation.lock().await;
                            let current = now();
                            let before_lease_count = owner.leases.read().await.len();
                            owner.leases.write().await.retain(|_, lease| lease.expires_at > current);
                            let leases_changed = owner.leases.read().await.len() != before_lease_count;
                            let has_active_lease = owner
                                .leases
                                .read()
                                .await
                                .values()
                                .any(|lease| lease.expires_at > current);
                            let policy = owner.policy().await;
                            let interval_ms = if has_active_lease {
                                policy.host_power_monitor_active_interval_ms
                            } else {
                                policy.host_power_monitor_idle_interval_ms
                            }
                            .max(1)
                            .min(i64::MAX as u64) as i64;
                            (leases_changed, interval_ms, current.millis())
                        };
                        let power_changed = if current_ms >= next_power_sample_ms {
                            next_power_sample_ms = current_ms.saturating_add(interval_ms);
                            tokio::select! {
                                _ = stop.cancelled() => {
                                    probe_stop.cancel();
                                    suspend_events.shutdown().await;
                                    return;
                                }
                                changed = owner.publish_power_sample() => changed,
                            }
                        } else {
                            false
                        };
                        if leases_changed || power_changed {
                            let _mutation = owner.mutation.lock().await;
                            let _ = owner.publish().await;
                        }
                    }
                }
            }
        }))
    }
}

const POWER_COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
const POWER_COMMAND_OUTPUT_LIMIT: u64 = 16 * 1024;

struct PowerProbe {
    source: HostPowerSource,
    idle: BackgroundBooleanState,
    idle_seconds: Option<u64>,
    locked: BackgroundBooleanState,
    // A running probe is evidence that the host is awake at this instant.
    // Suspend is therefore false only for a contacted sample; an uncontacted
    // sample is marked stale so callers cannot treat this field as observed.
    suspended: bool,
    on_battery: BackgroundBooleanState,
    low_power_mode: BackgroundBooleanState,
    thermal_state: agent_domain::HostPowerThermalState,
    speed_limit_percent: Option<u8>,
    contact: bool,
}

impl PowerProbe {
    fn unknown(source: HostPowerSource) -> Self {
        Self {
            source,
            idle: BackgroundBooleanState::Unknown,
            idle_seconds: None,
            locked: BackgroundBooleanState::Unknown,
            suspended: false,
            on_battery: BackgroundBooleanState::Unknown,
            low_power_mode: BackgroundBooleanState::Unknown,
            thermal_state: agent_domain::HostPowerThermalState::Unknown,
            speed_limit_percent: None,
            contact: false,
        }
    }
}

async fn read_power_output<R>(reader: R) -> Option<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    reader
        .take(POWER_COMMAND_OUTPUT_LIMIT + 1)
        .read_to_end(&mut output)
        .await
        .ok()?;
    (output.len() as u64 <= POWER_COMMAND_OUTPUT_LIMIT).then_some(output)
}

async fn stop_power_command(child: &mut tokio::process::Child) {
    let _ = tokio::time::timeout(POWER_COMMAND_TIMEOUT, child.kill()).await;
    let _ = tokio::time::timeout(POWER_COMMAND_TIMEOUT, child.wait()).await;
}

async fn finish_power_reader(
    reader: &mut tokio::task::JoinHandle<Option<Vec<u8>>>,
) -> Option<Vec<u8>> {
    match tokio::time::timeout(POWER_COMMAND_TIMEOUT, &mut *reader).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) | Err(_) => {
            reader.abort();
            None
        }
    }
}

struct PowerReaderTasks {
    stdout: Option<tokio::task::JoinHandle<Option<Vec<u8>>>>,
    stderr: Option<tokio::task::JoinHandle<Option<Vec<u8>>>>,
}

impl PowerReaderTasks {
    fn new(
        stdout: tokio::task::JoinHandle<Option<Vec<u8>>>,
        stderr: tokio::task::JoinHandle<Option<Vec<u8>>>,
    ) -> Self {
        Self {
            stdout: Some(stdout),
            stderr: Some(stderr),
        }
    }

    async fn collect(&mut self) -> Option<Vec<u8>> {
        let stdout = finish_power_reader(self.stdout.as_mut().expect("stdout reader exists"));
        let stderr = finish_power_reader(self.stderr.as_mut().expect("stderr reader exists"));
        let (stdout, _stderr) = tokio::join!(stdout, stderr);
        stdout
    }
}

impl Drop for PowerReaderTasks {
    fn drop(&mut self) {
        if let Some(stdout) = &self.stdout {
            stdout.abort();
        }
        if let Some(stderr) = &self.stderr {
            stderr.abort();
        }
    }
}

async fn wait_power_command(child: &mut tokio::process::Child, stop: &CancellationToken) -> bool {
    let deadline = Instant::now() + POWER_COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => {
                stop_power_command(child).await;
                return false;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_power_command(child).await;
            return false;
        }
        tokio::select! {
            _ = stop.cancelled() => {
                stop_power_command(child).await;
                return false;
            }
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(20))) => {}
        }
    }
}

/// Runs a short native helper without allowing it to retain a Tokio worker,
/// leak a child process, or grow an unbounded output buffer.  The caller owns
/// the cancellation token for its Host or desktop lifecycle.
async fn run_power_command(
    program: &str,
    args: &[&str],
    stop: &CancellationToken,
) -> Option<Vec<u8>> {
    if stop.is_cancelled() {
        return None;
    }
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let mut readers = PowerReaderTasks::new(
        tokio::spawn(read_power_output(stdout)),
        tokio::spawn(read_power_output(stderr)),
    );
    let status = wait_power_command(&mut child, stop).await;
    let stdout = readers.collect().await;
    if !status || stop.is_cancelled() {
        return None;
    }
    stdout
}

fn snapshot_from_power_probe(probe: PowerProbe) -> HostPowerSnapshot {
    HostPowerSnapshot {
        source: probe.source,
        idle: probe.idle,
        idle_seconds: probe.idle_seconds,
        locked: probe.locked,
        suspended: probe.suspended,
        on_battery: probe.on_battery,
        low_power_mode: probe.low_power_mode,
        thermal_state: probe.thermal_state,
        speed_limit_percent: probe.speed_limit_percent,
        desktop_processes: Vec::new(),
        stale: !probe.contact,
        updated_at: now(),
    }
}

async fn sample_power_probe(stop: &CancellationToken) -> PowerProbe {
    #[cfg(target_os = "linux")]
    {
        sample_linux_power(stop).await
    }
    #[cfg(target_os = "macos")]
    {
        sample_macos_power(stop).await
    }
    #[cfg(target_os = "windows")]
    {
        sample_windows_power(stop).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = stop;
        PowerProbe::unknown(HostPowerSource::Unknown)
    }
}

pub async fn sample_local_power(stop: &CancellationToken) -> HostPowerSnapshot {
    snapshot_from_power_probe(sample_power_probe(stop).await)
}

/// Desktop sampling uses the same native observations as the Host, but marks
/// the report so the Host can keep its own valid local observation authoritative
/// when this publisher is stale.
pub async fn sample_desktop_power(stop: &CancellationToken) -> HostPowerSnapshot {
    let mut snapshot = sample_local_power(stop).await;
    snapshot.source = HostPowerSource::DesktopMain;
    snapshot
}

#[cfg(target_os = "linux")]
async fn sample_linux_power(stop: &CancellationToken) -> PowerProbe {
    let mut probe = PowerProbe::unknown(HostPowerSource::NodeLinux);
    let (observed_power_supply, on_battery, speed_limit_percent) =
        tokio::task::spawn_blocking(sample_linux_native_power)
            .await
            .unwrap_or((false, BackgroundBooleanState::Unknown, None));
    probe.on_battery = on_battery;
    if let Some(output) = run_power_command("xprintidle", &[], stop).await
        && let Ok(output) = String::from_utf8(output)
        && let Ok(idle_ms) = output.trim().parse::<u64>()
    {
        probe.contact = true;
        probe.idle_seconds = Some(idle_ms / 1_000);
        probe.idle = if idle_ms >= 60_000 {
            BackgroundBooleanState::True
        } else {
            BackgroundBooleanState::False
        };
    }
    probe.speed_limit_percent = speed_limit_percent;
    probe.contact = linux_power_contact(
        observed_power_supply,
        probe.idle,
        probe.speed_limit_percent.is_some(),
    );
    probe
}

#[cfg(any(target_os = "linux", test))]
fn linux_power_contact(
    observed_power_supply: bool,
    idle: BackgroundBooleanState,
    speed_observed: bool,
) -> bool {
    observed_power_supply || idle != BackgroundBooleanState::Unknown || speed_observed
}

#[cfg(target_os = "linux")]
fn sample_linux_native_power() -> (bool, BackgroundBooleanState, Option<u8>) {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return (false, BackgroundBooleanState::Unknown, None);
    };
    let mut observed_power_supply = false;
    let mut power_supply_entries = false;
    let mut on_battery = BackgroundBooleanState::Unknown;
    for entry in entries.filter_map(Result::ok) {
        power_supply_entries = true;
        let status = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .map(|value| value.trim().to_ascii_lowercase());
        match status.as_deref() {
            Some("discharging") => {
                observed_power_supply = true;
                on_battery = BackgroundBooleanState::True;
            }
            Some("charging" | "full" | "not charging")
                if on_battery != BackgroundBooleanState::True =>
            {
                observed_power_supply = true;
                on_battery = BackgroundBooleanState::False;
            }
            _ => {}
        }
    }
    (
        observed_power_supply,
        on_battery,
        power_supply_entries
            .then(linux_speed_limit_percent)
            .flatten(),
    )
}

#[cfg(target_os = "linux")]
fn linux_speed_limit_percent() -> Option<u8> {
    let entries = std::fs::read_dir("/sys/devices/system/cpu").ok()?;
    let mut total = 0.0;
    let mut count = 0u64;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("cpu")
            || !name[3..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            continue;
        }
        let directory = entry.path().join("cpufreq");
        let max = std::fs::read_to_string(directory.join("cpuinfo_max_freq"))
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok());
        let limit = std::fs::read_to_string(directory.join("scaling_max_freq"))
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok());
        if let (Some(max), Some(limit)) = (max, limit)
            && max > 0.0
            && limit.is_finite()
        {
            total += (limit * 100.0 / max).clamp(0.0, 100.0);
            count += 1;
        }
    }
    (count > 0).then(|| (total / count as f64).round().clamp(0.0, 100.0) as u8)
}

async fn sample_macos_power(stop: &CancellationToken) -> PowerProbe {
    let mut probe = PowerProbe::unknown(HostPowerSource::NodeMacosShell);
    let (battery, therm, low_power, idle, lock, thermal_state) = tokio::join!(
        run_power_command("/usr/bin/pmset", &["-g", "batt"], stop),
        run_power_command("/usr/bin/pmset", &["-g", "therm"], stop),
        run_power_command("/usr/bin/pmset", &["-g", "custom"], stop),
        run_power_command("/usr/sbin/ioreg", &["-c", "IOHIDSystem", "-d", "4"], stop),
        run_power_command("/usr/sbin/ioreg", &["-n", "Root", "-d", "1", "-a"], stop),
        run_power_command(
            "/usr/sbin/sysctl",
            &["-n", "machdep.xcpm.cpu_thermal_level"],
            stop,
        ),
    );
    if let Some(output) = battery.and_then(|output| String::from_utf8(output).ok()) {
        probe.on_battery = parse_macos_battery_state(&output);
        probe.contact |= probe.on_battery != BackgroundBooleanState::Unknown;
    }
    if let Some(output) = therm.and_then(|output| String::from_utf8(output).ok()) {
        probe.speed_limit_percent = parse_macos_speed_limit(&output);
        probe.contact |= probe.speed_limit_percent.is_some();
    }
    if let Some(output) = low_power.and_then(|output| String::from_utf8(output).ok()) {
        probe.low_power_mode = parse_macos_low_power(&output);
        probe.contact |= probe.low_power_mode != BackgroundBooleanState::Unknown;
    }
    if let Some(output) = idle.and_then(|output| String::from_utf8(output).ok())
        && let Some(idle_ns) = parse_macos_idle_nanoseconds(&output)
    {
        let idle_ms = idle_ns / 1_000_000;
        probe.contact = true;
        probe.idle_seconds = Some(idle_ms / 1_000);
        probe.idle = if idle_ms >= 60_000 {
            BackgroundBooleanState::True
        } else {
            BackgroundBooleanState::False
        };
    }
    if let Some(output) = lock.and_then(|output| String::from_utf8(output).ok()) {
        probe.locked = parse_macos_locked(&output);
        probe.contact |= probe.locked != BackgroundBooleanState::Unknown;
    }
    if let Some(output) = thermal_state.and_then(|output| String::from_utf8(output).ok()) {
        probe.thermal_state = parse_macos_thermal_state(&output);
        probe.contact |= probe.thermal_state != agent_domain::HostPowerThermalState::Unknown;
    }
    probe
}

fn parse_macos_battery_state(output: &str) -> BackgroundBooleanState {
    if output.contains("Now drawing from 'Battery Power'") {
        BackgroundBooleanState::True
    } else if output.contains("Now drawing from 'AC Power'") {
        BackgroundBooleanState::False
    } else {
        BackgroundBooleanState::Unknown
    }
}

fn parse_macos_speed_limit(output: &str) -> Option<u8> {
    output.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("CPU_Speed_Limit") {
            return None;
        }
        value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| value.round().clamp(0.0, 100.0) as u8)
    })
}

fn parse_macos_low_power(output: &str) -> BackgroundBooleanState {
    let Some(value) = output.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let name = fields.next()?;
        let value = fields.next()?;
        name.eq_ignore_ascii_case("lowpowermode").then_some(value)
    }) else {
        return BackgroundBooleanState::Unknown;
    };
    parse_boolean_state(value)
}

fn parse_boolean_state(value: &str) -> BackgroundBooleanState {
    match value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'')
    {
        "1" | "true" | "on" | "yes" | "Yes" => BackgroundBooleanState::True,
        "0" | "false" | "off" | "no" | "No" => BackgroundBooleanState::False,
        _ => BackgroundBooleanState::Unknown,
    }
}

fn parse_macos_idle_nanoseconds(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        name.contains("HIDIdleTime")
            .then(|| {
                value
                    .trim()
                    .trim_matches(|character| character == '"' || character == '\'')
                    .parse::<u64>()
                    .ok()
            })
            .flatten()
    })
}

fn parse_macos_locked(output: &str) -> BackgroundBooleanState {
    if let Some(state) = parse_macos_xml_boolean(output, "CGSSessionScreenIsLocked") {
        return state;
    }
    output
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            if !(name.contains("CGSSessionScreenIsLocked") || name.contains("ScreenIsLocked")) {
                return None;
            }
            let state = parse_boolean_state(value);
            (state != BackgroundBooleanState::Unknown).then_some(state)
        })
        .unwrap_or(BackgroundBooleanState::Unknown)
}

fn parse_macos_xml_boolean(output: &str, key: &str) -> Option<BackgroundBooleanState> {
    let marker = format!("<key>{key}</key>");
    let value = output.split_once(&marker)?.1;
    let value = value.split("<key>").next().unwrap_or(value);
    if value.contains("<true") {
        Some(BackgroundBooleanState::True)
    } else if value.contains("<false") {
        Some(BackgroundBooleanState::False)
    } else {
        None
    }
}

fn parse_macos_thermal_state(output: &str) -> agent_domain::HostPowerThermalState {
    match output.trim().parse::<u32>() {
        Ok(0) => agent_domain::HostPowerThermalState::Nominal,
        Ok(1) => agent_domain::HostPowerThermalState::Fair,
        Ok(2) => agent_domain::HostPowerThermalState::Serious,
        Ok(_) => agent_domain::HostPowerThermalState::Critical,
        Err(_) => agent_domain::HostPowerThermalState::Unknown,
    }
}

#[cfg(target_os = "windows")]
fn sample_windows_native_power(probe: &mut PowerProbe) {
    use windows_sys::Win32::{
        System::{
            Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_SWITCHDESKTOP, GetUserObjectInformationW, HDESK,
                OpenInputDesktop, UOI_NAME,
            },
            SystemInformation::GetTickCount,
        },
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };

    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status) != 0 {
            probe.on_battery = parse_windows_ac_line_status(status.ACLineStatus);
            probe.low_power_mode = parse_windows_system_status(status.SystemStatusFlag);
            probe.contact = probe.on_battery != BackgroundBooleanState::Unknown
                || probe.low_power_mode != BackgroundBooleanState::Unknown;
        }

        let mut last_input = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut last_input) != 0 {
            probe.contact = true;
            let idle_ms = GetTickCount().wrapping_sub(last_input.dwTime) as u64;
            probe.idle_seconds = Some(idle_ms / 1_000);
            probe.idle = if idle_ms >= 60_000 {
                BackgroundBooleanState::True
            } else {
                BackgroundBooleanState::False
            };
        }

        let desktop: HDESK = OpenInputDesktop(0, 0, DESKTOP_SWITCHDESKTOP);
        if !desktop.is_null() {
            let mut name = [0u16; 64];
            let mut needed = 0u32;
            if GetUserObjectInformationW(
                desktop,
                UOI_NAME,
                name.as_mut_ptr().cast(),
                (name.len() * std::mem::size_of::<u16>()) as u32,
                &mut needed,
            ) != 0
            {
                let length = name
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(name.len());
                probe.locked = parse_windows_input_desktop_name(&name[..length]);
                probe.contact |= probe.locked != BackgroundBooleanState::Unknown;
            }
            let _ = CloseDesktop(desktop);
        }
    }
}

#[cfg(target_os = "windows")]
async fn sample_windows_power(_stop: &CancellationToken) -> PowerProbe {
    tokio::task::spawn_blocking(|| {
        let mut probe = PowerProbe::unknown(HostPowerSource::NodeWindows);
        sample_windows_native_power(&mut probe);
        probe
    })
    .await
    .unwrap_or_else(|_| PowerProbe::unknown(HostPowerSource::NodeWindows))
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_ac_line_status(value: u8) -> BackgroundBooleanState {
    match value {
        0 => BackgroundBooleanState::True,
        1 => BackgroundBooleanState::False,
        _ => BackgroundBooleanState::Unknown,
    }
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_system_status(value: u8) -> BackgroundBooleanState {
    match value {
        0 => BackgroundBooleanState::False,
        1 => BackgroundBooleanState::True,
        _ => BackgroundBooleanState::Unknown,
    }
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_input_desktop_name(name: &[u16]) -> BackgroundBooleanState {
    match name {
        [87, 105, 110, 108, 111, 103, 111, 110] => BackgroundBooleanState::True,
        [68, 101, 102, 97, 117, 108, 116] => BackgroundBooleanState::False,
        _ => BackgroundBooleanState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{BackgroundActivityProfile, BackgroundScope, ClientActivityReport};

    #[tokio::test]
    async fn power_monitor_drops_heartbeats_and_rejects_old_reports() {
        let monitor = HostPowerMonitor::new();
        let at = now();
        let initial = HostPowerSnapshot {
            stale: false,
            ..unknown_power(at.clone())
        };
        assert!(monitor.report(initial.clone()).await);
        let heartbeat_at = Timestamp::from_millis(at.millis() + 1).unwrap();
        assert!(
            !monitor
                .report(HostPowerSnapshot {
                    idle_seconds: Some(10),
                    updated_at: heartbeat_at.clone(),
                    ..initial.clone()
                })
                .await
        );
        assert_eq!(monitor.snapshot().await.updated_at, heartbeat_at);
        assert!(
            !monitor
                .report(HostPowerSnapshot {
                    locked: BackgroundBooleanState::True,
                    updated_at: Timestamp::from_millis(at.millis() - 1).unwrap(),
                    ..initial.clone()
                })
                .await
        );
        assert_eq!(
            monitor.snapshot().await.locked,
            BackgroundBooleanState::Unknown
        );
    }

    #[tokio::test]
    async fn lifecycle_events_are_authoritative_until_a_matching_resume() {
        let monitor = HostPowerMonitor::new();
        let at = now();
        let initial = HostPowerSnapshot {
            source: native_power_source(),
            stale: false,
            updated_at: at.clone(),
            ..unknown_power(at.clone())
        };
        assert!(monitor.report(initial).await);

        let suspended_at = Timestamp::from_millis(at.millis() + 1).unwrap();
        assert!(
            monitor
                .report_lifecycle_at(true, suspended_at.clone())
                .await
        );
        let suspended = monitor.snapshot().await;
        assert!(suspended.suspended);
        assert!(!suspended.stale);
        assert!(host_power_constrained(
            &suspended,
            &BackgroundActivityPolicy::preset(BackgroundActivityProfile::Balanced),
        ));

        let awake_sample = HostPowerSnapshot {
            source: native_power_source(),
            stale: false,
            suspended: false,
            updated_at: Timestamp::from_millis(suspended_at.millis() + 1).unwrap(),
            ..unknown_power(suspended_at.clone())
        };
        assert!(!monitor.report(awake_sample).await);
        assert!(monitor.snapshot().await.suspended);

        assert!(
            monitor
                .report_lifecycle_at(
                    false,
                    Timestamp::from_millis(suspended_at.millis() + 2).unwrap(),
                )
                .await
        );
        let resumed = monitor.snapshot().await;
        assert!(!resumed.suspended);
        assert!(!resumed.stale);
    }

    #[tokio::test]
    async fn owner_stop_cancels_the_lifecycle_source() {
        let directory = tempfile::tempdir().unwrap();
        let owner = BackgroundOwner::new(directory.path().to_owned());
        let stop = CancellationToken::new();
        let mut task = owner.spawn(stop.clone());
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), &mut task)
            .await
            .expect("background owner did not stop")
            .expect("background owner task failed");
        assert!(owner.probe_stop.is_cancelled());
    }

    #[tokio::test]
    async fn desktop_power_receiver_reports_native_publisher_health() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let at = now();
        let snapshot = HostPowerSnapshot {
            source: HostPowerSource::DesktopMain,
            stale: false,
            on_battery: BackgroundBooleanState::False,
            updated_at: at.clone(),
            ..unknown_power(at)
        };
        owner.report_power(snapshot, true).await;
        let telemetry = owner.resources.snapshot(owner.power.snapshot().await);
        assert_eq!(
            telemetry.health.desktop.status,
            ResourceSourceStatus::Healthy
        );
        assert_eq!(telemetry.health.desktop.last_error, None);
    }

    #[tokio::test]
    async fn stale_desktop_power_cannot_mask_a_fresh_host_observation() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let at = now();
        let host = HostPowerSnapshot {
            source: HostPowerSource::NodeLinux,
            stale: false,
            updated_at: at.clone(),
            ..unknown_power(at.clone())
        };
        owner.power.report(host).await;
        owner
            .report_power(
                HostPowerSnapshot {
                    source: HostPowerSource::DesktopMain,
                    stale: true,
                    updated_at: Timestamp::from_millis(at.millis() + 1).unwrap(),
                    ..unknown_power(at)
                },
                true,
            )
            .await;
        assert_eq!(
            owner.power.snapshot().await.source,
            HostPowerSource::NodeLinux
        );
        assert!(!owner.power.snapshot().await.stale);
    }

    #[tokio::test]
    async fn remote_desktop_publisher_is_rejected_by_the_host_owner() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let at = now();
        owner
            .report_power(
                HostPowerSnapshot {
                    source: HostPowerSource::DesktopMain,
                    stale: false,
                    updated_at: Timestamp::from_millis(at.millis() + 86_400_000).unwrap(),
                    ..unknown_power(at)
                },
                false,
            )
            .await;
        let snapshot = owner.power.snapshot().await;
        assert_eq!(snapshot.source, HostPowerSource::Unknown);
        assert!(snapshot.stale);
    }

    #[tokio::test]
    async fn remote_native_power_report_is_rejected_even_with_a_host_source_label() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let at = now();
        owner
            .report_power(
                HostPowerSnapshot {
                    source: HostPowerSource::NodeLinux,
                    stale: false,
                    on_battery: BackgroundBooleanState::True,
                    updated_at: at.clone(),
                    ..unknown_power(at)
                },
                false,
            )
            .await;
        let snapshot = owner.power.snapshot().await;
        assert_eq!(snapshot.source, HostPowerSource::Unknown);
        assert!(snapshot.stale);
    }

    #[tokio::test]
    async fn host_normalizes_future_power_timestamps_and_keeps_desktop_health_scoped() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let now_before = now().millis();
        let future = Timestamp::from_millis(now_before + 86_400_000).unwrap();
        owner
            .report_power(
                HostPowerSnapshot {
                    source: HostPowerSource::NodeLinux,
                    stale: false,
                    updated_at: future,
                    ..unknown_power(Timestamp::from_millis(now_before).unwrap())
                },
                true,
            )
            .await;
        let stored = owner.power.snapshot().await;
        assert!(stored.updated_at.millis() >= now_before);
        assert!(stored.updated_at.millis() < now_before + 10_000);
        assert_eq!(
            owner.resources.snapshot(stored).health.desktop.status,
            ResourceSourceStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn node_power_reports_cannot_publish_desktop_process_rows() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let at = now();
        let monitor = DesktopProcessMonitor::new();
        owner
            .report_power(
                HostPowerSnapshot {
                    source: HostPowerSource::NodeLinux,
                    stale: false,
                    desktop_processes: monitor.sample(),
                    updated_at: at.clone(),
                    ..unknown_power(at)
                },
                true,
            )
            .await;
        assert!(owner.power.snapshot().await.desktop_processes.is_empty());
    }

    #[tokio::test]
    async fn activity_delete_is_scoped_to_session_and_rpc_client() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let observed_at = now();
        let report = ClientActivityReport {
            environment_id: None,
            client_id: "client".into(),
            client_kind: agent_domain::BackgroundClientKind::Web,
            visible: true,
            focused: true,
            recently_interacted: true,
            app_state: Some(agent_domain::BackgroundAppState::Active),
            low_power_mode: Some(BackgroundBooleanState::Unknown),
            battery_state: Some(agent_domain::BackgroundBatteryState::Unknown),
            network_type: None,
            scopes: vec![BackgroundScope::Diagnostics],
            ttl_ms: Some(60_000),
            observed_at: observed_at.clone(),
        };
        owner
            .report_activity(
                1,
                agent_protocol::background::ReportClientActivity {
                    rpc_client_id: 4,
                    report: report.clone(),
                },
            )
            .await
            .unwrap();
        owner
            .report_activity(
                2,
                agent_protocol::background::ReportClientActivity {
                    rpc_client_id: 4,
                    report,
                },
            )
            .await
            .unwrap();
        let snapshot = owner.remove_activity(1, 4).await;
        assert_eq!(snapshot.leases.len(), 1);
        assert_eq!(snapshot.leases[0].session_id, "session:2");
        assert_eq!(
            owner.policy().await.profile,
            BackgroundActivityProfile::Balanced
        );
    }

    #[tokio::test]
    async fn activity_report_uses_host_time_for_expiry_and_recovery() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let report = ClientActivityReport {
            environment_id: None,
            client_id: "client".into(),
            client_kind: agent_domain::BackgroundClientKind::Web,
            visible: true,
            focused: true,
            recently_interacted: true,
            app_state: Some(agent_domain::BackgroundAppState::Active),
            low_power_mode: Some(BackgroundBooleanState::Unknown),
            battery_state: Some(agent_domain::BackgroundBatteryState::Unknown),
            network_type: None,
            scopes: vec![BackgroundScope::Diagnostics],
            ttl_ms: Some(60_000),
            observed_at: Timestamp::from_millis(0).unwrap(),
        };
        let before = now().millis();
        let snapshot = owner
            .report_activity(
                1,
                agent_protocol::background::ReportClientActivity {
                    rpc_client_id: 4,
                    report,
                },
            )
            .await
            .unwrap();
        let lease = snapshot.leases.first().expect("activity lease");
        assert!(lease.updated_at.millis() >= before);
        assert!(lease.expires_at.millis() > lease.updated_at.millis());
    }

    #[tokio::test]
    async fn active_leases_gate_vcs_provider_and_diagnostic_consumers() {
        let directory = tempfile::tempdir().unwrap();
        let owner = BackgroundOwner::new(directory.path().to_owned());
        let report = ClientActivityReport {
            environment_id: None,
            client_id: "client".into(),
            client_kind: agent_domain::BackgroundClientKind::DesktopRenderer,
            visible: true,
            focused: true,
            recently_interacted: true,
            app_state: Some(agent_domain::BackgroundAppState::Active),
            low_power_mode: Some(BackgroundBooleanState::False),
            battery_state: Some(agent_domain::BackgroundBatteryState::Full),
            network_type: None,
            scopes: vec![
                BackgroundScope::VcsStatus {
                    cwd: "/repo".into(),
                },
                BackgroundScope::ProviderStatus { instance_id: None },
                BackgroundScope::Diagnostics,
            ],
            ttl_ms: Some(60_000),
            observed_at: now(),
        };
        owner
            .report_activity(
                1,
                agent_protocol::background::ReportClientActivity {
                    rpc_client_id: 2,
                    report,
                },
            )
            .await
            .unwrap();
        assert_eq!(owner.demanded_vcs_workspaces().await, vec!["/repo"]);
        assert!(owner.has_provider_status_demand().await);
        owner.sample_resources_if_demanded().await;
        assert!(owner.process_diagnostics().await.read_at.millis() > 0);
    }

    #[tokio::test]
    async fn policy_recovers_from_state_directory() {
        let directory = tempfile::tempdir().unwrap();
        let owner = BackgroundOwner::new(directory.path().to_owned());
        owner
            .set_policy(BackgroundActivityPolicy::preset(
                BackgroundActivityProfile::Performance,
            ))
            .await;

        let recovered = BackgroundOwner::new(directory.path().to_owned());
        assert_eq!(
            recovered.policy().await.profile,
            BackgroundActivityProfile::Performance
        );
        assert_eq!(
            recovered.policy().await.automatic_git_fetch_interval_ms,
            15_000
        );
    }

    #[test]
    fn history_deltas_are_cumulative_and_cutoff_bounded() {
        assert_eq!(history_delta(100, Some(40), 1_000), 60);
        assert_eq!(scale_history_delta(60, 0.5), 30);
        assert_eq!(history_delta(100, Some(40), 30_001), 0);
        assert_eq!(history_delta(40, Some(100), 1_000), 0);
    }

    #[test]
    fn native_process_probe_keeps_real_host_identity_and_capacity() {
        let owner = ResourceOwner::new();
        let snapshot = owner.snapshot(unknown_power(now()));
        assert!(
            snapshot
                .processes
                .iter()
                .any(|process| process.category == ResourceProcessCategory::Server)
        );
        let resources = owner.host_resources();
        assert!(resources.usable_for_load_balancing());
        assert!(snapshot.health.scanned_process_count >= snapshot.processes.len() as u64);
        assert!(snapshot.processes.len() <= RESOURCE_PROCESS_LIMIT);
        assert!(
            snapshot
                .processes
                .iter()
                .all(|process| process.name.chars().count() <= RESOURCE_TEXT_LIMIT)
        );
        let history = owner.state.lock().unwrap();
        assert!(history.snapshots.len() <= RESOURCE_HISTORY_LIMIT);
        assert!(resource_history_entries(&history.snapshots) <= RESOURCE_HISTORY_MAX_ENTRIES);
        assert!(resource_history_bytes(&history.snapshots) <= RESOURCE_HISTORY_MAX_BYTES);
    }

    #[test]
    fn desktop_process_monitor_reports_the_gpui_root_process() {
        let monitor = DesktopProcessMonitor::new();
        let processes = monitor.sample();
        assert!(processes.iter().any(|process| {
            process.identity.pid == std::process::id()
                && process.category == ResourceProcessCategory::Unknown
        }));
    }

    #[tokio::test]
    async fn desktop_process_rows_are_retained_when_host_power_is_local() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let monitor = DesktopProcessMonitor::new();
        let at = now();
        owner
            .report_power(
                HostPowerSnapshot {
                    source: HostPowerSource::DesktopMain,
                    stale: false,
                    on_battery: BackgroundBooleanState::False,
                    desktop_processes: monitor.sample(),
                    updated_at: at.clone(),
                    ..unknown_power(at)
                },
                true,
            )
            .await;
        let telemetry = owner.resources.snapshot(owner.power.snapshot().await);
        assert!(
            telemetry
                .processes
                .iter()
                .any(|process| process.category == ResourceProcessCategory::Unknown)
        );
    }

    #[test]
    fn process_tree_selection_rejects_reused_pid_identities() {
        let rows = vec![(10, 1, 100), (11, 10, 90), (12, 10, 101), (13, 12, 102)];
        let selected = select_tracked_pids(&rows, 10);
        assert_eq!(selected.into_iter().collect::<Vec<_>>(), vec![10, 12, 13]);
    }

    #[test]
    fn attribution_records_are_aggregated_and_sorted() {
        let owner = ResourceOwner::new();
        owner.record_attribution("git", "remote.fetch", 11, 5, 1, 7);
        owner.record_attribution("git", "remote.fetch", 13, 9, 2, 3);
        owner.record_attribution("provider", "health.refresh", 50, 0, 1, 1);
        let snapshot = owner.snapshot(unknown_power(now()));
        assert_eq!(snapshot.attribution.entries[0].component, "git");
        assert_eq!(snapshot.attribution.entries[0].logical_read_bytes, 24);
        assert_eq!(snapshot.attribution.entries[0].logical_write_bytes, 14);
        assert_eq!(snapshot.attribution.entries[0].count, 3);
        assert_eq!(snapshot.attribution.entries[0].duration_ms, 10);
    }

    #[test]
    fn platform_power_parsers_keep_unknown_when_output_is_unavailable() {
        assert_eq!(
            parse_macos_battery_state("Now drawing from 'AC Power'"),
            BackgroundBooleanState::False
        );
        assert_eq!(
            parse_macos_battery_state("unexpected"),
            BackgroundBooleanState::Unknown
        );
        assert_eq!(parse_macos_speed_limit("CPU_Speed_Limit = 65\n"), Some(65));
        assert_eq!(
            parse_macos_low_power("lowpowermode 1\n"),
            BackgroundBooleanState::True
        );
    }

    #[tokio::test]
    async fn desktop_power_probe_marks_gpui_publisher_source_without_carrying_unknown_fields() {
        let stop = CancellationToken::new();
        let snapshot = sample_desktop_power(&stop).await;
        assert_eq!(snapshot.source, HostPowerSource::DesktopMain);
        assert!(snapshot.updated_at.millis() > 0);
    }

    #[test]
    fn platform_power_parsers_report_native_observations_without_defaults() {
        assert!(!linux_power_contact(
            false,
            BackgroundBooleanState::Unknown,
            false
        ));
        assert!(linux_power_contact(
            true,
            BackgroundBooleanState::Unknown,
            false
        ));
        assert_eq!(
            parse_macos_idle_nanoseconds("\"HIDIdleTime\" = 60000000000"),
            Some(60_000_000_000)
        );
        assert_eq!(
            parse_macos_locked(
                "<?xml version=\"1.0\"?><plist><dict><key>CGSSessionScreenIsLocked</key><true/></dict></plist>",
            ),
            BackgroundBooleanState::True
        );
        assert_eq!(
            parse_macos_locked("\"CGSSessionScreenIsLocked\" = Yes"),
            BackgroundBooleanState::True
        );
        assert_eq!(
            parse_macos_locked("no lock field"),
            BackgroundBooleanState::Unknown
        );
        assert_eq!(
            parse_macos_thermal_state("2"),
            agent_domain::HostPowerThermalState::Serious
        );
        assert_eq!(
            parse_macos_thermal_state("unavailable"),
            agent_domain::HostPowerThermalState::Unknown
        );
        assert_eq!(
            parse_boolean_state("unknown"),
            BackgroundBooleanState::Unknown
        );
        assert_eq!(
            parse_windows_ac_line_status(0),
            BackgroundBooleanState::True
        );
        assert_eq!(parse_windows_system_status(1), BackgroundBooleanState::True);
        assert_eq!(
            parse_windows_input_desktop_name(&[87, 105, 110, 108, 111, 103, 111, 110]),
            BackgroundBooleanState::True
        );
        assert_eq!(
            parse_windows_input_desktop_name(&[68, 101, 102, 97, 117, 108, 116]),
            BackgroundBooleanState::False
        );
    }

    #[test]
    fn desktop_freshness_requires_a_known_power_field_and_host_timestamp() {
        let at = now();
        let unknown = HostPowerSnapshot {
            source: HostPowerSource::DesktopMain,
            stale: false,
            updated_at: at.clone(),
            ..unknown_power(at.clone())
        };
        assert!(!desktop_power_sample_is_fresh(&unknown, &at));
        let observed = HostPowerSnapshot {
            on_battery: BackgroundBooleanState::False,
            ..unknown
        };
        assert!(desktop_power_sample_is_fresh(&observed, &at));
        let future = Timestamp::from_millis(at.millis() + 1).unwrap();
        assert!(!desktop_power_sample_is_fresh(
            &HostPowerSnapshot {
                updated_at: future,
                ..observed
            },
            &at,
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn power_command_cancellation_and_output_limits_cleanup_the_child() {
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(
            run_power_command("sh", &["-c", "printf ignored"], &stop)
                .await
                .is_none()
        );

        let stop = CancellationToken::new();
        assert!(
            run_power_command("sh", &["-c", "head -c 20000 /dev/zero"], &stop,)
                .await
                .is_none()
        );
    }
}
