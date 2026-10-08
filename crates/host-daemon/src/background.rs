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
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{Mutex as TokioMutex, RwLock, broadcast};
use tokio_util::sync::CancellationToken;

const RESOURCE_HISTORY_LIMIT: usize = 720;
const TRACE_MAX_FILES: u32 = 16;

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
    previous_cpu: Option<(u64, u64)>,
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
                previous_cpu: None,
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
        let host = sample_host_resources(&self.state);
        let mut processes = sample_processes(&sampled_at);
        let attribution = ResourceAttributionSnapshot {
            read_at: sampled_at.clone(),
            entries: vec![],
        };
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
        state.health.native.status = ResourceSourceStatus::Healthy;
        state.health.native.last_sample_at = Some(sampled_at.clone());
        state.health.native.last_error = None;
        state.health.retained_process_count = processes.len() as u64;
        state.health.scanned_process_count = processes.len() as u64;
        let groups = resource_groups(&processes, &state.lifecycle);
        let snapshot = ResourceTelemetrySnapshot {
            read_at: sampled_at,
            sample_interval_ms: 5_000,
            processes,
            groups,
            power,
            speed_limit_percent: None,
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
}

fn read_cpu_counters() -> Option<(u64, u64)> {
    let line = std::fs::read_to_string("/proc/stat").ok()?.lines().next()?.to_owned();
    let mut values = line.split_whitespace().skip(1).filter_map(|value| value.parse::<u64>().ok());
    let user = values.next()?;
    let nice = values.next()?;
    let system = values.next()?;
    let idle = values.next()?;
    let iowait = values.next().unwrap_or(0);
    let irq = values.next().unwrap_or(0);
    let softirq = values.next().unwrap_or(0);
    let steal = values.next().unwrap_or(0);
    Some((idle + iowait, user + nice + system + idle + iowait + irq + softirq + steal))
}

fn sample_host_resources(state: &Mutex<ResourceState>) -> agent_domain::HostResourcesSnapshot {
    let sampled_at = now().millis().max(0) as u64;
    let mut total_memory_bytes = 0;
    let mut available_memory_bytes = 0;
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else { continue; };
            let value = parts.next().and_then(|value| value.parse::<u64>().ok()).unwrap_or(0) * 1024;
            match name {
                "MemTotal:" => total_memory_bytes = value,
                "MemAvailable:" => available_memory_bytes = value,
                _ => {}
            }
        }
    }
    let cpu = read_cpu_counters();
    let cpu_utilization = if let Some((idle, total)) = cpu {
        let mut guard = state.lock().unwrap_or_else(|error| error.into_inner());
        let value = guard.previous_cpu.replace((idle, total)).and_then(|(old_idle, old_total)| {
            let total_delta = total.saturating_sub(old_total);
            let idle_delta = idle.saturating_sub(old_idle);
            (total_delta > 0).then(|| (1.0 - idle_delta as f64 / total_delta as f64).clamp(0.0, 1.0))
        });
        value
    } else {
        None
    };
    agent_domain::HostResourcesSnapshot {
        sampled_at,
        cpu_utilization,
        cpu_count: std::thread::available_parallelism().map(|value| value.get() as u64).unwrap_or(0),
        available_memory_bytes: available_memory_bytes.min(total_memory_bytes),
        total_memory_bytes,
    }
}

fn sample_processes(at: &Timestamp) -> Vec<ResourceProcess> {
    #[cfg(target_os = "linux")]
    {
        if let Some(processes) = sample_linux_processes(at) {
            return processes;
        }
    }
    vec![sample_server_process(at)]
}

fn sample_server_process(at: &Timestamp) -> ResourceProcess {
    let pid = std::process::id();
    let command = std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| "host".into());
    let (resident_bytes, virtual_bytes) = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|value| {
            let mut parts = value.split_whitespace().filter_map(|part| part.parse::<u64>().ok());
            Some((parts.next()?, parts.next()?))
        })
        .map(|(virtual_pages, resident_pages)| (resident_pages * 4096, virtual_pages * 4096))
        .unwrap_or((0, 0));
    ResourceProcess {
        identity: ResourceProcessIdentity { pid, start_time_ms: 0 },
        ppid: 0,
        child_pids: vec![],
        depth: 0,
        name: "host".into(),
        command,
        status: "Running".into(),
        category: ResourceProcessCategory::Server,
        cpu_percent: 0.0,
        cpu_time_ms: 0,
        resident_bytes,
        peak_resident_bytes: resident_bytes,
        virtual_bytes,
        io_read_bytes: 0,
        io_write_bytes: 0,
        io_read_bytes_per_second: 0.0,
        io_write_bytes_per_second: 0.0,
        io_semantics: ResourceTelemetryIoSemantics::Unavailable,
        run_time_ms: 0,
        first_seen_at: at.clone(),
        last_seen_at: at.clone(),
    }
}

#[cfg(target_os = "linux")]
fn process_io(path: &Path) -> Option<(u64, u64)> {
    let Ok(contents) = std::fs::read_to_string(path.join("io")) else {
        return None;
    };
    let mut read_bytes = 0;
    let mut write_bytes = 0;
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue; };
        let Some(value) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
            continue;
        };
        match name {
            "read_bytes:" => read_bytes = value,
            "write_bytes:" => write_bytes = value,
            _ => {}
        }
    }
    Some((read_bytes, write_bytes))
}

#[cfg(target_os = "linux")]
fn sample_linux_processes(at: &Timestamp) -> Option<Vec<ResourceProcess>> {
    let root_pid = std::process::id();
    let mut processes = Vec::new();
    let uptime_ms = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|value| value.split_whitespace().next()?.parse::<f64>().ok())
        .map(|seconds| (seconds.max(0.0) * 1_000.0) as u64);
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(pid) = name.to_string_lossy().parse::<u32>().ok() else { continue; };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue; };
        let Some(close) = stat.rfind(") ") else { continue; };
        let Some(fields_text) = stat.get(close + 2..) else { continue; };
        let fields = fields_text.split_whitespace().collect::<Vec<_>>();
        let status = fields.first().copied().unwrap_or("?").to_owned();
        let ppid = fields.get(1).and_then(|value| value.parse().ok()).unwrap_or(0);
        let start_ticks = fields.get(19).and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
        let cpu_ticks = fields.get(11).and_then(|value| value.parse::<u64>().ok()).unwrap_or(0)
            + fields.get(12).and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
        let command = std::fs::read(entry.path().join("cmdline"))
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " ").trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| stat[1..close].to_owned());
        let process_path = entry.path();
        let (virtual_bytes, resident_bytes) = std::fs::read_to_string(process_path.join("statm"))
            .ok()
            .and_then(|value| {
                let mut values = value.split_whitespace().filter_map(|part| part.parse::<u64>().ok());
                Some((values.next()? * 4096, values.next()? * 4096))
            })
            .unwrap_or((0, 0));
        let io = process_io(&process_path);
        let io_available = io.is_some();
        let (io_read_bytes, io_write_bytes) = io.unwrap_or((0, 0));
        let start_time_ms = start_ticks.saturating_mul(10);
        let run_time_ms = uptime_ms
            .map(|uptime| uptime.saturating_sub(start_time_ms))
            .unwrap_or(0);
        let lower = command.to_ascii_lowercase();
        let category = if pid == root_pid {
            ResourceProcessCategory::Server
        } else if lower.contains("provider") {
            ResourceProcessCategory::ProviderRoot
        } else if lower.contains("terminal") || lower.contains("pty") {
            ResourceProcessCategory::TerminalRoot
        } else if lower.contains("electron") && lower.contains("renderer") {
            ResourceProcessCategory::ElectronRenderer
        } else if lower.contains("electron") && lower.contains("gpu") {
            ResourceProcessCategory::ElectronGpu
        } else if lower.contains("electron") && lower.contains("utility") {
            ResourceProcessCategory::ElectronUtility
        } else if lower.contains("electron") {
            ResourceProcessCategory::ElectronMain
        } else if lower.contains("resource-monitor") || lower.contains("resource_monitor") {
            ResourceProcessCategory::ResourceMonitor
        } else {
            ResourceProcessCategory::ServerChild
        };
        processes.push(ResourceProcess {
            identity: ResourceProcessIdentity {
                pid,
                start_time_ms,
            },
            ppid,
            child_pids: vec![],
            depth: 0,
            name: stat[1..close].to_owned(),
            command,
            status,
            category,
            cpu_percent: 0.0,
            cpu_time_ms: cpu_ticks.saturating_mul(10),
            resident_bytes,
            peak_resident_bytes: resident_bytes,
            virtual_bytes,
            io_read_bytes,
            io_write_bytes,
            io_read_bytes_per_second: 0.0,
            io_write_bytes_per_second: 0.0,
            io_semantics: if io_available {
                ResourceTelemetryIoSemantics::Storage
            } else {
                ResourceTelemetryIoSemantics::Unavailable
            },
            run_time_ms,
            first_seen_at: at.clone(),
            last_seen_at: at.clone(),
        });
    }
    let mut parent_map = BTreeMap::new();
    for process in &processes {
        parent_map.insert(process.identity.pid, process.ppid);
    }
    let retained: std::collections::BTreeSet<_> = processes
        .iter()
        .filter_map(|process| {
            let mut current = process.identity.pid;
            for _ in 0..64 {
                if current == root_pid {
                    return Some(process.identity.pid);
                }
                let parent = parent_map.get(&current).copied()?;
                if parent == current || parent == 0 {
                    break;
                }
                current = parent;
            }
            None
        })
        .collect();
    processes.retain(|process| retained.contains(&process.identity.pid));
    for process in &mut processes {
        process.depth = if process.identity.pid == root_pid { 0 } else { 1 };
        let mut parent = process.ppid;
        while parent != 0 && parent != root_pid && process.depth < 64 {
            process.depth += 1;
            parent = parent_map.get(&parent).copied().unwrap_or(0);
        }
    }
    for index in 0..processes.len() {
        let pid = processes[index].identity.pid;
        processes[index].child_pids = processes
            .iter()
            .filter(|candidate| candidate.ppid == pid)
            .map(|candidate| candidate.identity.pid)
            .collect();
    }
    Some(processes)
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
        let sampled = sample_power(&current);
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
        sample_host_resources(&self.resources.state)
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
                            Ok(Some(line)) => aggregator.add_line(&line),
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
            return Ok(agent_domain::empty_trace_diagnostics(path.to_string_lossy(), paths, read_at, slow_span_threshold_ms, failure.unwrap_or(TraceDiagnosticsError { kind: TraceDiagnosticsErrorKind::TraceFileNotFound, message: "No local trace files were found.".into() })));
        }
        let partial_failure = failure.as_ref().map(|_| true);
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
                        owner.leases.write().await.retain(|_, lease| lease.expires_at > current);
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
                        if current.millis() >= next_power_sample_ms {
                            next_power_sample_ms = current.millis().saturating_add(interval_ms);
                            let _ = owner.publish_power_sample().await;
                        }
                        let _ = owner.publish().await;
                    }
                }
            }
        }))
    }
}

fn sample_power(previous: &HostPowerSnapshot) -> HostPowerSnapshot {
    let at = now();
    let source = if cfg!(target_os = "linux") {
        HostPowerSource::NodeLinux
    } else if cfg!(target_os = "windows") {
        HostPowerSource::NodeWindows
    } else if cfg!(target_os = "macos") {
        HostPowerSource::NodeMacosShell
    } else {
        HostPowerSource::Unknown
    };
    let on_battery = if cfg!(target_os = "linux") {
        let status = std::fs::read_dir("/sys/class/power_supply")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("status")).ok())
            .map(|value| value.trim().to_ascii_lowercase())
            .find(|value| ["charging", "discharging", "full"].contains(&value.as_str()))
            .map(|value| if value == "discharging" { BackgroundBooleanState::True } else { BackgroundBooleanState::False })
            .unwrap_or(BackgroundBooleanState::Unknown)
    } else {
        previous.on_battery
    };
    HostPowerSnapshot {
        source,
        idle: previous.idle,
        idle_seconds: previous.idle_seconds,
        locked: previous.locked,
        suspended: previous.suspended,
        on_battery,
        low_power_mode: previous.low_power_mode,
        thermal_state: previous.thermal_state,
        stale: false,
        updated_at: at,
    }
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
}
