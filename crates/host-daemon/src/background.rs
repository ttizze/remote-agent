//! Host-owned background policy, power probes, local resource samples, and
//! trace diagnostics.  The domain crate supplies all decisions and views.
use agent_domain::{
    BackgroundActivityPolicy, BackgroundPolicySnapshot, BackgroundScope, BackgroundBooleanState,
    HostPowerSnapshot, HostPowerSource, ResourceAggregate,
    ResourceAttributionSnapshot, ResourceHealth, ResourceProcess, ResourceProcessCategory,
    ResourceProcessIdentity, ResourceTelemetryIoSemantics, ResourceTelemetrySnapshot,
    ResourceTelemetryHistory, ResourceHistoryBucket, ResourceProcessSummary, ResourceSourceHealth,
    ResourceSourceStatus, Timestamp, TraceDiagnosticsAggregator, TraceDiagnosticsError,
    TraceDiagnosticsErrorKind, TraceDiagnosticsResult, ClientActivityReport, ClientActivityLease,
    compute_background_snapshot, host_power_constrained,
    lease_may_run_scoped_work, remove_rpc_client, upsert_client_activity_lease,
    normalize_resource_history_window, process_diagnostics, project_process_resource_history,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{Mutex as TokioMutex, RwLock, broadcast};
use tokio_util::sync::CancellationToken;

const RESOURCE_HISTORY_LIMIT: usize = 720;
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

/// Semantic host-power state.  Idle seconds are retained in the snapshot but
/// do not cause a policy publication by themselves.
pub(crate) struct HostPowerMonitor {
    latest: RwLock<HostPowerSnapshot>,
}
impl HostPowerMonitor {
    fn new() -> Self {
        Self {
            latest: RwLock::new(unknown_power(now())),
        }
    }

    async fn snapshot(&self) -> HostPowerSnapshot {
        self.latest.read().await.clone()
    }

    async fn report(&self, next: HostPowerSnapshot) -> bool {
        let mut latest = self.latest.write().await;
        if next.updated_at < latest.updated_at {
            return false;
        }
        if latest.same_state(&next) {
            if next.updated_at > latest.updated_at {
                *latest = next;
            }
            return false;
        }
        *latest = next;
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
            let elapsed_ms = sampled_at
                .millis()
                .saturating_sub(*previous_at);
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
                process.io_write_bytes_per_second = write_delta as f64 * 1_000.0 / elapsed_ms as f64;
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
            read_at: sampled_at,
            sample_interval_ms: 5_000,
            processes,
            groups,
            power,
            speed_limit_percent,
            attribution,
            health: state.health.clone(),
        };
        state.snapshots.push_back(snapshot.clone());
        while state.snapshots.len() > RESOURCE_HISTORY_LIMIT {
            state.snapshots.pop_front();
        }
        snapshot
    }

    fn latest_history(&self, power: HostPowerSnapshot, window_ms: u64, bucket_ms: u64) -> ResourceTelemetryHistory {
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
            .filter(|sample| sample.read_at.millis() < cutoff)
            .next_back();
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
                        (sample
                            .read_at
                            .millis()
                            .saturating_sub(cutoff) as f64
                            / elapsed as f64)
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
        if let (Some(snapshot), Some(refreshed_at)) =
            (state.host_resources.clone(), state.host_resources_refreshed_at)
            && refreshed_at.elapsed() < Duration::from_secs(5)
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
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let unavailable = snapshot.stale || snapshot.source == HostPowerSource::Unknown;
        state.health.desktop.status = if unavailable {
            ResourceSourceStatus::Unavailable
        } else {
            ResourceSourceStatus::Healthy
        };
        state.health.desktop.last_sample_at = Some(snapshot.updated_at.clone());
        state.health.desktop.last_error = unavailable.then(|| {
            "The desktop power publisher did not provide a fresh native observation.".into()
        });
    }
}

fn expire_desktop_health(health: &mut ResourceHealth, at: &Timestamp) {
    if health.desktop.last_sample_at.as_ref().is_some_and(|sampled| {
        at.millis().saturating_sub(sampled.millis()) > DESKTOP_POWER_HEALTH_TIMEOUT_MS
    }) {
        health.desktop.status = ResourceSourceStatus::Unavailable;
        health.desktop.last_error = Some(
            "The desktop power publisher has not reported a fresh observation.".into(),
        );
    }
}

fn sample_host_resources(
    state: &mut ResourceState,
    sampled_at: &Timestamp,
) -> agent_domain::HostResourcesSnapshot {
    state.system.refresh_memory();
    let total_memory_bytes = state.system.total_memory();
    let mut available_memory_bytes = state.system.available_memory();
    #[cfg(target_os = "linux")]
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        if let Some(available) = parse_meminfo_bytes(&meminfo, "MemAvailable:") {
            available_memory_bytes = available;
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = std::process::Command::new("/usr/bin/vm_stat")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    {
        if let Ok(output) = String::from_utf8(output.stdout) {
            if let Some(available) = darwin_available_memory(&output) {
                available_memory_bytes = available;
            }
        }
    }

    let previous_cpu_at = state.host_cpu_refreshed_at.replace(Instant::now());
    state.system.refresh_cpu_usage();
    let cpu_utilization = previous_cpu_at
        .filter(|at| at.elapsed() >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL)
        .map(|_| (state.system.global_cpu_usage() as f64 / 100.0).clamp(0.0, 1.0));
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

fn darwin_available_memory(output: &str) -> Option<u64> {
    let page_size = output
        .lines()
        .find_map(|line| line.strip_prefix("Mach Virtual Memory Statistics: (page size of "))
        .and_then(|value| value.split_once(" bytes)")?.0.parse::<u64>().ok())
        .filter(|page_size| *page_size > 0)?;
    let page_count = ["Pages free:", "Pages inactive:", "Pages speculative:"]
        .into_iter()
        .map(|name| {
            output.lines().find_map(|line| {
                let rest = line.strip_prefix(name)?.trim();
                rest.trim_end_matches('.').parse::<u64>().ok()
            })
        })
        .collect::<Option<Vec<_>>>()?;
    page_count
        .into_iter()
        .try_fold(0u64, |total, pages| total.checked_add(pages.checked_mul(page_size)?))
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

    let previous_at = state.snapshots.back().map(|snapshot| snapshot.read_at.millis());
    let mut processes = Vec::with_capacity(selected.len());
    for pid in selected {
        let Some(process) = state.system.process(sysinfo::Pid::from_u32(pid)) else {
            continue;
        };
        let start_time_ms = process.start_time().saturating_mul(1_000);
        let identity = ResourceProcessIdentity { pid, start_time_ms };
        let identity_key = identity.key();
        let cpu_percent = f64::from(process.cpu_usage()).max(0.0);
        let cpu_time_ms = state
            .process_cpu_time_ms
            .get(&identity_key)
            .copied()
            .unwrap_or(0)
            .saturating_add(
                previous_at
                    .and_then(|previous| {
                        let elapsed = at.millis().saturating_sub(previous);
                        (elapsed > 0 && elapsed <= MAX_RESOURCE_DELTA_INTERVAL_MS).then(|| {
                            (cpu_percent * elapsed as f64 / 100.0)
                                .round()
                                .max(0.0) as u64
                        })
                    })
                    .unwrap_or(0),
            );
        state
            .process_cpu_time_ms
            .insert(identity_key, cpu_time_ms);
        let name = process.name().to_string_lossy().into_owned();
        let command = if process.cmd().is_empty() {
            name.clone()
        } else {
            process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
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
            status: format!("{:?}", process.status()),
            category,
            cpu_percent: 0.0,
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
    state.process_cpu_time_ms.retain(|key, _| {
        processes.iter().any(|process| process.identity.key() == *key)
    });
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
    let inaccessible_process_count = selected_process_count(&rows, root_pid)
        .saturating_sub(processes.len()) as u64;
    let (status, error) = if inaccessible_process_count > 0 {
        (
            ResourceSourceStatus::Degraded,
            Some(format!(
                "{} descendant process(es) could not be read.",
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

fn record_attribution(
    entries: &mut BTreeMap<(String, String), agent_domain::ResourceAttributionEntry>,
    component: &str,
    operation: &str,
    logical_read_bytes: u64,
    logical_write_bytes: u64,
    count: u64,
    duration_ms: u64,
) {
    let key = (component.to_owned(), operation.to_owned());
    let entry = entries.entry(key).or_insert_with(|| agent_domain::ResourceAttributionEntry {
        component: component.to_owned(),
        operation: operation.to_owned(),
        logical_read_bytes: 0,
        logical_write_bytes: 0,
        count: 0,
        duration_ms: 0,
    });
    entry.logical_read_bytes = entry.logical_read_bytes.saturating_add(logical_read_bytes);
    entry.logical_write_bytes = entry.logical_write_bytes.saturating_add(logical_write_bytes);
    entry.count = entry.count.saturating_add(count);
    entry.duration_ms = entry.duration_ms.saturating_add(duration_ms);
}

fn attribution_snapshot(
    state: &ResourceState,
    read_at: Timestamp,
) -> ResourceAttributionSnapshot {
    let mut entries = state.attribution.values().cloned().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .logical_write_bytes
            .cmp(&left.logical_write_bytes)
            .then_with(|| right.logical_read_bytes.cmp(&left.logical_read_bytes))
            .then_with(|| left.component.cmp(&right.component))
            .then_with(|| left.operation.cmp(&right.operation))
    });
    ResourceAttributionSnapshot { read_at, entries }
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
        children.entry(*ppid).or_default().push((*pid, *start_time_ms));
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
    (delta as f64 * fraction).round().clamp(0.0, u64::MAX as f64) as u64
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
    let mut io_read_bytes = 0;
    let mut io_write_bytes = 0;
    let processes = current
        .processes
        .iter()
        .map(|process| {
            let previous = previous_processes
                .as_ref()
                .and_then(|processes| processes.get(&process.identity.key()).copied());
            let cpu_time_ms = scale_history_delta(history_delta(
                process.cpu_time_ms,
                previous.map(|process| process.cpu_time_ms),
                elapsed_ms,
            ), delta_fraction);
            let io_read = scale_history_delta(history_delta(
                process.io_read_bytes,
                previous.map(|process| process.io_read_bytes),
                elapsed_ms,
            ), delta_fraction);
            let io_write = scale_history_delta(history_delta(
                process.io_write_bytes,
                previous.map(|process| process.io_write_bytes),
                elapsed_ms,
            ), delta_fraction);
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
        let entry = summaries.entry(process.identity.key()).or_insert_with(|| ResourceProcessSummary {
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

fn aggregate_values(processes: impl Iterator<Item = &ResourceProcess>) -> ResourceAggregate {
    let mut aggregate = ResourceAggregate::default();
    for process in processes {
        aggregate.process_count += 1;
        aggregate.current_cpu_percent += process.cpu_percent;
        aggregate.cpu_time_ms = aggregate.cpu_time_ms.saturating_add(process.cpu_time_ms);
        aggregate.current_rss_bytes = aggregate.current_rss_bytes.saturating_add(process.resident_bytes);
        aggregate.peak_rss_bytes = aggregate.peak_rss_bytes.saturating_add(process.peak_resident_bytes);
        aggregate.io_read_bytes = aggregate.io_read_bytes.saturating_add(process.io_read_bytes);
        aggregate.io_write_bytes = aggregate.io_write_bytes.saturating_add(process.io_write_bytes);
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
            let io_write_bytes = process.io_write_bytes.saturating_sub(previous.io_write_bytes);
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

    pub(crate) async fn set_policy(&self, policy: BackgroundActivityPolicy) -> BackgroundPolicySnapshot {
        let _mutation = self.mutation.lock().await;
        let policy = policy.normalized();
        *self.policy.write().await = policy.clone();
        if let Err(error) = crate::platform::save_private_json(&self.policy_path, &policy) {
            tracing::warn!(target: "bex", operation = "background.policy.persist", message = %error);
        }
        self.publish().await
    }

    pub(crate) async fn report_power(&self, snapshot: HostPowerSnapshot) -> BackgroundPolicySnapshot {
        let _mutation = self.mutation.lock().await;
        let previous = self.power.snapshot().await;
        let accepted = snapshot.updated_at >= previous.updated_at;
        if accepted {
            self.resources.record_desktop_power(&snapshot);
        }
        if self.power.report(snapshot).await {
            self.publish().await
        } else {
            self.snapshot_at(&now()).await
        }
    }

    pub(crate) async fn has_demand(&self, scope: &BackgroundScope) -> bool {
        let snapshot = self.snapshot().await;
        snapshot.active_scope_keys.contains(&agent_domain::scope_key(scope))
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

    /// A cheap gate for owner-managed opportunistic refreshes.
    pub(crate) async fn should_run_opportunistic_work(&self) -> bool {
        self.snapshot().await.should_run_opportunistic_work
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
            let _ = self.resources.snapshot(self.power.snapshot().await);
        }
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
        let current = self.power.snapshot().await;
        if current.source == HostPowerSource::DesktopMain
            && !current.stale
            && now()
                .millis()
                .saturating_sub(current.updated_at.millis())
                <= DESKTOP_POWER_HEALTH_TIMEOUT_MS
        {
            return false;
        }
        let sampled = sample_local_power(&current);
        self.power.report(sampled).await
    }

    pub(crate) async fn report_activity(&self, session: u64, input: agent_protocol::background::ReportClientActivity) -> Result<BackgroundPolicySnapshot, String> {
        let _mutation = self.mutation.lock().await;
        let policy = self.policy().await;
        let now = now();
        let report = input.report;
        let lease = report
            .lease_at(format!("session:{session}"), input.rpc_client_id, &policy, &now)
            .map_err(str::to_owned)?;
        let mut leases = self.leases.write().await;
        let next = upsert_client_activity_lease(&leases, lease, &now);
        *leases = next;
        drop(leases);
        Ok(self.publish().await)
    }

    pub(crate) async fn remove_activity(&self, session: u64, rpc_client_id: u64) -> BackgroundPolicySnapshot {
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
    ) -> (broadcast::Receiver<BackgroundPolicySnapshot>, BackgroundPolicySnapshot) {
        let _mutation = self.mutation.lock().await;
        let snapshot = self.snapshot().await;
        (self.changes.subscribe(), snapshot)
    }

    pub(crate) async fn host_resources(&self) -> agent_domain::HostResourcesSnapshot {
        self.resources.host_resources()
    }

    pub(crate) fn record_attribution(
        &self,
        component: &str,
        operation: &str,
        logical_read_bytes: u64,
        logical_write_bytes: u64,
        count: u64,
        duration_ms: u64,
    ) {
        self.resources.record_attribution(
            component,
            operation,
            logical_read_bytes,
            logical_write_bytes,
            count,
            duration_ms,
        );
    }

    pub(crate) async fn process_diagnostics(&self) -> agent_domain::ProcessDiagnosticsResult {
        let snapshot = self.resources.snapshot(self.power.snapshot().await);
        process_diagnostics(
            std::process::id(),
            snapshot.read_at.clone(),
            &snapshot.processes,
            snapshot.health.native.last_error.map(|message| {
                agent_domain::ProcessDiagnosticsError { message }
            }),
        )
    }

    pub(crate) async fn process_history(&self, window_ms: u64, bucket_ms: u64) -> agent_domain::ProcessResourceHistoryResult {
        let history = self.resources.latest_history(self.power.snapshot().await, window_ms, bucket_ms);
        project_process_resource_history(&history)
    }

    pub(crate) async fn trace_diagnostics(&self, input: &agent_protocol::background::ReadTraceDiagnostics) -> Result<TraceDiagnosticsResult, String> {
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
                                logical_read_bytes = logical_read_bytes
                                    .saturating_add(line.len() as u64);
                                aggregator.add_line(&line);
                            }
                            Ok(None) => break,
                            Err(_error) => {
                                if failure.is_none() {
                                    failure = Some(TraceDiagnosticsError {
                                        kind: TraceDiagnosticsErrorKind::TraceFileReadFailed,
                                        message: format!("Failed to read local trace file '{scanned}'."),
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
            );
            return Ok(agent_domain::empty_trace_diagnostics(path.to_string_lossy(), paths, read_at, slow_span_threshold_ms, failure.unwrap_or(TraceDiagnosticsError { kind: TraceDiagnosticsErrorKind::TraceFileNotFound, message: "No local trace files were found.".into() })));
        }
        let partial_failure = failure.as_ref().map(|_| true);
        self.record_attribution(
            "diagnostics",
            "trace.read",
            logical_read_bytes,
            0,
            1,
            trace_started.elapsed().as_millis() as u64,
        );
        Ok(aggregator.finish(path.to_string_lossy(), paths, read_at, failure, partial_failure))
    }

    fn allowed_trace_path(&self, requested: &str) -> Result<PathBuf, String> {
        let default = self.state_directory.join("logs/host.jsonl");
        let path = if requested.trim().is_empty() { default } else { PathBuf::from(requested) };
        let parent = path.parent().ok_or_else(|| "trace path has no parent".to_owned())?;
        let logs = self.state_directory.join("logs");
        let parent = dunce::canonicalize(parent).map_err(|_| "trace path is unavailable".to_owned())?;
        let logs = dunce::canonicalize(logs).map_err(|_| "trace directory is unavailable".to_owned())?;
        if !parent.starts_with(logs) {
            return Err("trace path must be inside the Host diagnostics directory".into());
        }
        if let Ok(canonical) = dunce::canonicalize(&path) {
            if !canonical.starts_with(&logs) {
                return Err("trace path must be inside the Host diagnostics directory".into());
            }
        }
        Ok(path)
    }

    pub(crate) fn spawn(self: &Arc<Self>, stop: CancellationToken) -> tokio_util::task::AbortOnDropHandle<()> {
        let owner = Arc::downgrade(self);
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut next_power_sample_ms = now().millis();
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = interval.tick() => {
                        let Some(owner) = owner.upgrade() else { return; };
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
                        let power_changed = if current.millis() >= next_power_sample_ms {
                            next_power_sample_ms = current.millis().saturating_add(interval_ms);
                            owner.publish_power_sample().await
                        } else {
                            false
                        };
                        if leases_changed || power_changed {
                            let _ = owner.publish().await;
                        }
                    }
                }
            }
        }))
    }
}

struct PowerProbe {
    source: HostPowerSource,
    idle: BackgroundBooleanState,
    idle_seconds: Option<u64>,
    locked: BackgroundBooleanState,
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

pub fn sample_local_power(previous: &HostPowerSnapshot) -> HostPowerSnapshot {
    let at = now();
    let probe = if cfg!(target_os = "linux") {
        sample_linux_power()
    } else if cfg!(target_os = "windows") {
        sample_windows_power()
    } else if cfg!(target_os = "macos") {
        sample_macos_power()
    } else {
        PowerProbe::unknown(HostPowerSource::Unknown)
    };
    let mut snapshot = HostPowerSnapshot {
        source: probe.source,
        idle: probe.idle,
        idle_seconds: probe.idle_seconds,
        locked: probe.locked,
        suspended: probe.suspended,
        on_battery: probe.on_battery,
        low_power_mode: probe.low_power_mode,
        thermal_state: probe.thermal_state,
        speed_limit_percent: probe.speed_limit_percent,
        stale: !probe.contact,
        updated_at: at,
    };
    carry_forward_unobserved_power(previous, &mut snapshot);
    snapshot
}

/// The GPUI desktop uses the same native probe implementation as the Host,
/// while retaining a source identity so the Host does not replace a fresh
/// desktop observation with its own fallback sample.
pub fn sample_desktop_power(previous: &HostPowerSnapshot) -> HostPowerSnapshot {
    let mut snapshot = sample_local_power(previous);
    snapshot.source = HostPowerSource::DesktopMain;
    snapshot
}

fn carry_forward_unobserved_power(previous: &HostPowerSnapshot, next: &mut HostPowerSnapshot) {
    if next.stale {
        return;
    }
    if next.idle == BackgroundBooleanState::Unknown {
        next.idle = previous.idle;
        next.idle_seconds = previous.idle_seconds;
    }
    if next.locked == BackgroundBooleanState::Unknown {
        next.locked = previous.locked;
    }
    if next.on_battery == BackgroundBooleanState::Unknown {
        next.on_battery = previous.on_battery;
    }
    if next.low_power_mode == BackgroundBooleanState::Unknown {
        next.low_power_mode = previous.low_power_mode;
    }
    if next.thermal_state == agent_domain::HostPowerThermalState::Unknown {
        next.thermal_state = previous.thermal_state;
    }
    if next.speed_limit_percent.is_none() {
        next.speed_limit_percent = previous.speed_limit_percent;
    }
}

fn sample_linux_power() -> PowerProbe {
    let mut probe = PowerProbe::unknown(HostPowerSource::NodeLinux);
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return probe;
    };
    probe.contact = true;
    for entry in entries.filter_map(Result::ok) {
        let status = std::fs::read_to_string(entry.path().join("status"))
            .ok()
            .map(|value| value.trim().to_ascii_lowercase());
        match status.as_deref() {
            Some("discharging") => probe.on_battery = BackgroundBooleanState::True,
            Some("charging" | "full" | "not charging")
                if probe.on_battery != BackgroundBooleanState::True =>
            {
                probe.on_battery = BackgroundBooleanState::False;
            }
            _ => {}
        }
    }
    let idle = std::process::Command::new("xprintidle")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| output.trim().parse::<u64>().ok());
    if let Some(idle_ms) = idle {
        probe.idle_seconds = Some(idle_ms / 1_000);
        probe.idle = if idle_ms >= 60_000 {
            BackgroundBooleanState::True
        } else {
            BackgroundBooleanState::False
        };
    }
    probe.speed_limit_percent = linux_speed_limit_percent();
    probe
}

fn linux_speed_limit_percent() -> Option<u8> {
    let entries = std::fs::read_dir("/sys/devices/system/cpu").ok()?;
    let mut total = 0.0;
    let mut count = 0u64;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("cpu") || !name[3..].chars().all(|character| character.is_ascii_digit()) {
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

fn sample_macos_power() -> PowerProbe {
    let mut probe = PowerProbe::unknown(HostPowerSource::NodeMacosShell);
    let battery = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "batt"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok();
    if let Some(output) = battery.filter(|output| output.status.success()) {
        probe.contact = true;
        if let Ok(output) = String::from_utf8(output.stdout) {
            probe.on_battery = parse_macos_battery_state(&output);
        }
    }
    let thermal = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "therm"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok();
    if let Some(output) = thermal.filter(|output| output.status.success())
        && let Ok(output) = String::from_utf8(output.stdout)
    {
        probe.contact = true;
        probe.speed_limit_percent = parse_macos_speed_limit(&output);
    }
    let low_power = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "custom"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok();
    if let Some(output) = low_power.filter(|output| output.status.success())
        && let Ok(output) = String::from_utf8(output.stdout)
    {
        probe.contact = true;
        probe.low_power_mode = parse_macos_low_power(&output);
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
    match value {
        "1" | "true" | "on" => BackgroundBooleanState::True,
        "0" | "false" | "off" => BackgroundBooleanState::False,
        _ => BackgroundBooleanState::Unknown,
    }
}

fn sample_windows_power() -> PowerProbe {
    let mut probe = PowerProbe::unknown(HostPowerSource::NodeWindows);
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_Battery | Select-Object -ExpandProperty BatteryStatus) -join ','",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok();
    if let Some(output) = output.filter(|output| output.status.success()) {
        probe.contact = true;
        if let Ok(output) = String::from_utf8(output.stdout) {
            probe.on_battery = parse_windows_battery_state(&output);
        }
    }
    let speed = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_Processor | ForEach-Object { if ($_.MaxClockSpeed -gt 0) { $_.CurrentClockSpeed * 100 / $_.MaxClockSpeed } }) -join ','",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok();
    if let Some(output) = speed.filter(|output| output.status.success())
        && let Ok(output) = String::from_utf8(output.stdout)
    {
        probe.contact = true;
        probe.speed_limit_percent = parse_windows_speed_limit(&output);
    }
    probe
}

fn parse_windows_battery_state(output: &str) -> BackgroundBooleanState {
    let mut saw_battery = false;
    let mut discharging = false;
    for value in output.split(',').filter_map(|value| value.trim().parse::<u32>().ok()) {
        saw_battery = true;
        discharging |= matches!(value, 1 | 4 | 5 | 11);
    }
    if !saw_battery {
        BackgroundBooleanState::Unknown
    } else if discharging {
        BackgroundBooleanState::True
    } else {
        BackgroundBooleanState::False
    }
}

fn parse_windows_speed_limit(output: &str) -> Option<u8> {
    let values = output
        .split(',')
        .filter_map(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0);
    let (total, count) = values.fold((0.0, 0u64), |(total, count), value| {
        (total + value.clamp(0.0, 100.0), count + 1)
    });
    (count > 0).then(|| (total / count as f64).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{BackgroundActivityProfile, BackgroundScope};

    #[tokio::test]
    async fn power_monitor_drops_heartbeats_and_rejects_old_reports() {
        let monitor = HostPowerMonitor::new();
        let at = now();
        let initial = HostPowerSnapshot { stale: false, ..unknown_power(at.clone()) };
        assert!(monitor.report(initial.clone()).await);
        let heartbeat_at = Timestamp::from_millis(at.millis() + 1).unwrap();
        assert!(!monitor.report(HostPowerSnapshot { idle_seconds: Some(10), updated_at: heartbeat_at.clone(), ..initial.clone() }).await);
        assert_eq!(monitor.snapshot().await.updated_at, heartbeat_at);
        assert!(!monitor.report(HostPowerSnapshot { locked: BackgroundBooleanState::True, updated_at: Timestamp::from_millis(at.millis() - 1).unwrap(), ..initial.clone() }).await);
        assert_eq!(monitor.snapshot().await.locked, BackgroundBooleanState::Unknown);
    }

    #[tokio::test]
    async fn desktop_power_receiver_reports_native_publisher_health() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let at = now();
        let snapshot = HostPowerSnapshot {
            source: HostPowerSource::DesktopMain,
            stale: false,
            updated_at: at.clone(),
            ..unknown_power(at)
        };
        owner.report_power(snapshot).await;
        let telemetry = owner.resources.snapshot(owner.power.snapshot().await);
        assert_eq!(telemetry.health.desktop.status, ResourceSourceStatus::Healthy);
        assert_eq!(telemetry.health.desktop.last_error, None);
    }

    #[tokio::test]
    async fn activity_delete_is_scoped_to_session_and_rpc_client() {
        let owner = BackgroundOwner::new(std::env::temp_dir());
        let observed_at = now();
        let report = ClientActivityReport {
            environment_id: None, client_id: "client".into(), client_kind: agent_domain::BackgroundClientKind::Web,
            visible: true, focused: true, recently_interacted: true, app_state: Some(agent_domain::BackgroundAppState::Active),
            low_power_mode: Some(BackgroundBooleanState::Unknown), battery_state: Some(agent_domain::BackgroundBatteryState::Unknown),
            network_type: None, scopes: vec![BackgroundScope::Diagnostics], ttl_ms: Some(60_000), observed_at: observed_at.clone(),
        };
        owner.report_activity(1, agent_protocol::background::ReportClientActivity { rpc_client_id: 4, report: report.clone() }).await.unwrap();
        owner.report_activity(2, agent_protocol::background::ReportClientActivity { rpc_client_id: 4, report: report }).await.unwrap();
        let snapshot = owner.remove_activity(1, 4).await;
        assert_eq!(snapshot.leases.len(), 1);
        assert_eq!(snapshot.leases[0].session_id, "session:2");
        assert_eq!(owner.policy().await.profile, BackgroundActivityProfile::Balanced);
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
                BackgroundScope::VcsStatus { cwd: "/repo".into() },
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
            .set_policy(BackgroundActivityPolicy::preset(BackgroundActivityProfile::Performance))
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
        assert!(snapshot
            .processes
            .iter()
            .any(|process| process.category == ResourceProcessCategory::Server));
        let resources = owner.host_resources();
        assert!(resources.usable_for_load_balancing());
        assert!(snapshot.health.scanned_process_count >= snapshot.processes.len() as u64);
    }

    #[test]
    fn process_tree_selection_rejects_reused_pid_identities() {
        let rows = vec![
            (10, 1, 100),
            (11, 10, 90),
            (12, 10, 101),
            (13, 12, 102),
        ];
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
            parse_macos_battery_state("Now drawing from 'AC Power'") ,
            BackgroundBooleanState::False
        );
        assert_eq!(
            parse_macos_battery_state("unexpected"),
            BackgroundBooleanState::Unknown
        );
        assert_eq!(
            parse_windows_battery_state(""),
            BackgroundBooleanState::Unknown
        );
        assert_eq!(parse_windows_speed_limit("50, 75\n"), Some(63));
        assert_eq!(
            parse_macos_speed_limit("CPU_Speed_Limit = 65\n"),
            Some(65)
        );
        assert_eq!(
            parse_macos_low_power("lowpowermode 1\n"),
            BackgroundBooleanState::True
        );
    }

    #[test]
    fn power_sampling_keeps_known_event_state_when_a_probe_lacks_that_field() {
        let at = Timestamp::from_millis(10).unwrap();
        let previous = HostPowerSnapshot {
            locked: BackgroundBooleanState::True,
            low_power_mode: BackgroundBooleanState::True,
            thermal_state: agent_domain::HostPowerThermalState::Serious,
            speed_limit_percent: Some(65),
            ..unknown_power(at.clone())
        };
        let mut next = HostPowerSnapshot {
            source: HostPowerSource::NodeLinux,
            stale: false,
            updated_at: Timestamp::from_millis(20).unwrap(),
            ..unknown_power(at)
        };
        carry_forward_unobserved_power(&previous, &mut next);
        assert_eq!(next.locked, BackgroundBooleanState::True);
        assert_eq!(next.low_power_mode, BackgroundBooleanState::True);
        assert_eq!(next.thermal_state, agent_domain::HostPowerThermalState::Serious);
        assert_eq!(next.speed_limit_percent, Some(65));
    }

    #[test]
    fn desktop_power_probe_marks_gpui_publisher_source() {
        let at = Timestamp::from_millis(10).unwrap();
        let snapshot = sample_desktop_power(&unknown_power(at));
        assert_eq!(snapshot.source, HostPowerSource::DesktopMain);
    }

    #[test]
    fn darwin_memory_parser_requires_all_reclaimable_pages() {
        let output = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: 2.\nPages inactive: 3.\nPages speculative: 1.\n";
        assert_eq!(darwin_available_memory(output), Some(24_576));
        assert_eq!(darwin_available_memory("Pages free: 2."), None);
    }
}
